//! Search engine (qBittorrent `base/search`): nova3 Python plugin management
//! (install from URL/file, enable/disable, update from the official index),
//! running `nova2.py` jobs and parsing their pipe-separated output, and
//! downloading results through `nova2dl.py`.
//!
//! Layout: [`nova`] installs the bundled helper scripts and reads plugin
//! version headers, [`python`] finds the interpreter, [`plugins`] parses
//! plugin metadata, [`proxy`] builds the proxy environment, [`process`]
//! spawns children safely and [`job`] runs one search.
//!
//! Plugin operations (install, update, uninstall, capability refresh,
//! Python re-detection, settings writes) run in order on one worker task, so
//! the synchronous facade methods never block on disk or process I/O.

mod job;
mod nova;
mod plugins;
mod process;
mod proxy;
mod python;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use self::job::{Job, JobLaunch, cap_text};
use self::nova::{NovaDirs, PluginVersion, VersionHeader};
use self::plugins::{PluginEntry, is_valid_plugin_name};
use self::process::python_command;
use self::proxy::ProxyEnv;
use self::python::{PythonInfo, PythonVersion};
use crate::bittorrent::Session;
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;
use crate::net::http::{self, HttpPurpose, HttpRequest};
use crate::utils::fs::write_atomic;

/// Official plugin repository (`SearchPluginManager::m_updateUrl`).
const UPDATE_URL: &str = "https://raw.githubusercontent.com/qbittorrent/search-plugins/refs/heads/master/nova3/engines/";
/// Concurrently running searches (the WebAPI allows 5 per session).
pub(crate) const MAX_RUNNING_JOBS: usize = 5;
/// Searches kept (finished ones stay until deleted); the oldest finished
/// search is dropped when a new one would exceed this.
pub(crate) const MAX_JOBS: usize = 50;
const MAX_PATTERN_CHARS: usize = 1024;
const MAX_PLUGINS_PER_JOB: usize = 256;
const MAX_TORRENT_URL_LEN: usize = 64 * 1024;
const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(30);
const SYNTAX_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const NOVA2DL_TIMEOUT: Duration = Duration::from_secs(120);
const VERSIONS_TXT_MAX_SIZE: u64 = 1024 * 1024;
const ICON_MAX_SIZE: u64 = 1024 * 1024;
/// Minimum delay between automatic Python re-detections while none is found.
const REDETECT_INTERVAL: Duration = Duration::from_secs(30);
const ERR_NO_PYTHON: &str = "Python must be installed to use the Search Engine.";

/// Compiles a file without running it (plugin syntax validation).
const SYNTAX_CHECK: &str = "import sys\nwith open(sys.argv[1], 'rb') as f:\n    compile(f.read(), sys.argv[1], 'exec')\n";

/// Adds a torrent to the session (abstracted so tests need no session).
pub(crate) type AddFuture = Pin<Box<dyn Future<Output = QbtResult<String>> + Send>>;
pub(crate) type TorrentAdder = Arc<dyn Fn(TorrentSource, AddTorrentParams) -> AddFuture + Send + Sync>;

/// `config/search.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct SearchConfig {
    /// qBittorrent `SearchEngines/disabledEngines`.
    disabled_engines: Vec<String>,
}

struct PythonState {
    info: Option<PythonInfo>,
    last_attempt: Instant,
    redetect_pending: bool,
}

enum Op {
    Install { sources: Vec<String>, reply: oneshot::Sender<QbtResult<()>> },
    Update { reply: oneshot::Sender<QbtResult<()>> },
    Uninstall { names: Vec<String> },
    SaveConfig,
    DetectPython { quiet: bool },
    Refresh,
    #[cfg(test)]
    Barrier { reply: oneshot::Sender<()> },
}

/// Why one plugin could not be installed.
struct InstallFailure {
    name: String,
    /// The plugin was installed before (update rather than install).
    update: bool,
    reason: String,
}

struct Inner {
    ctx: CoreContext,
    dirs: NovaDirs,
    config_file: PathBuf,
    update_url: String,
    search_timeout: Duration,
    fetch_icons: bool,
    adder: TorrentAdder,
    python: RwLock<PythonState>,
    /// Installed plugins by id (sorted).
    plugins: RwLock<BTreeMap<String, PluginEntry>>,
    disabled: Mutex<BTreeSet<String>>,
    jobs: Mutex<Vec<Arc<Job>>>,
    ops: mpsc::UnboundedSender<Op>,
    icon_attempted: Mutex<HashSet<String>>,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// Settings that differ between the application and tests.
pub(crate) struct Options {
    pub update_url: String,
    pub search_timeout: Duration,
    pub fetch_icons: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { update_url: UPDATE_URL.to_string(), search_timeout: job::SEARCH_TIMEOUT, fetch_icons: true }
    }
}

pub struct SearchManager {
    inner: Arc<Inner>,
}

impl SearchManager {
    pub async fn launch(ctx: &CoreContext, session: Arc<Session>) -> QbtResult<Arc<Self>> {
        let adder: TorrentAdder = Arc::new(move |source, params| {
            let session = session.clone();
            // Downloaded by a plugin: never a trusted local source.
            Box::pin(async move { session.add_remote_torrent(source, params).await })
        });
        Self::launch_with(ctx, adder, Options::default()).await
    }

    pub(crate) async fn launch_with(ctx: &CoreContext, adder: TorrentAdder, options: Options) -> QbtResult<Arc<Self>> {
        let dirs = NovaDirs::new(ctx.profile.search_dir());
        if let Err(e) = nova::update_nova(&dirs) {
            ctx.logger.warning(format!("Failed to install the search engine files to \"{}\". Error: \"{e}\".", dirs.root.display()));
        }
        let config_file = ctx.profile.config_dir.join("search.json");
        let disabled = load_config(&config_file, ctx);
        let preferred = ctx.prefs.current().advanced.python_executable_path.clone();
        let info = python::detect(&preferred, &ctx.logger, true).await;

        let (ops, ops_rx) = mpsc::unbounded_channel();
        let (shutdown, _) = watch::channel(false);
        let inner = Arc::new(Inner {
            ctx: ctx.clone(),
            dirs,
            config_file,
            update_url: options.update_url,
            search_timeout: options.search_timeout,
            fetch_icons: options.fetch_icons,
            adder,
            python: RwLock::new(PythonState { info, last_attempt: Instant::now(), redetect_pending: false }),
            plugins: RwLock::new(BTreeMap::new()),
            disabled: Mutex::new(disabled),
            jobs: Mutex::new(Vec::new()),
            ops,
            icon_attempted: Mutex::new(HashSet::new()),
            shutdown,
            tasks: Mutex::new(Vec::new()),
        });
        let worker = ctx.rt.spawn(worker(inner.clone(), ops_rx));
        let watcher = ctx.rt.spawn(watch_prefs(inner.clone()));
        inner.tasks.lock().extend([worker, watcher]);
        let _ = inner.ops.send(Op::Refresh);
        Ok(Arc::new(Self { inner }))
    }

    /// Stops every search (killing its processes) and the background tasks.
    pub async fn shutdown(&self) {
        let inner = &self.inner;
        inner.shutdown.send_replace(true);
        let jobs: Vec<Arc<Job>> = std::mem::take(&mut *inner.jobs.lock());
        for j in &jobs {
            j.request_stop();
        }
        let mut handles: Vec<JoinHandle<()>> = jobs.iter().filter_map(|j| j.task.lock().take()).collect();
        handles.extend(inner.tasks.lock().drain(..));
        for mut h in handles {
            if tokio::time::timeout(Duration::from_secs(5), &mut h).await.is_err() {
                // Dropping the task kills its process group (GroupGuard).
                h.abort();
                let _ = h.await;
            }
        }
    }

    // ---- Python / plugins -----------------------------------------------------

    pub fn status(&self) -> SearchEngineStatus {
        let status = {
            let st = self.inner.python.read();
            match &st.info {
                Some(i) if i.is_supported() => SearchEngineStatus {
                    python_found: true,
                    python_path: i.path.to_string_lossy().into_owned(),
                    python_version: i.version.to_string(),
                    message: String::new(),
                },
                Some(i) => SearchEngineStatus {
                    python_found: false,
                    python_path: i.path.to_string_lossy().into_owned(),
                    python_version: i.version.to_string(),
                    message: format!(
                        "Your Python version ({}) is outdated. Please upgrade to latest version for search engines to work.\nMinimum requirement: {}.",
                        i.version,
                        PythonVersion::MINIMUM
                    ),
                },
                None => SearchEngineStatus {
                    message: "Python is required to use the search engine but it does not seem to be installed.".into(),
                    ..Default::default()
                },
            }
        };
        if !status.python_found {
            self.inner.maybe_redetect();
        }
        status
    }

    pub fn plugins(&self) -> Vec<SearchPlugin> {
        let list: Vec<SearchPlugin> = self.inner.plugins.read().values().map(PluginEntry::to_model).collect();
        self.inner.schedule_icon_fetch();
        list
    }

    /// Installs (or updates) plugins from http(s) URLs or local `.py` paths.
    /// Fails with every per-plugin error when any source failed; a summary is
    /// also sent as `SearchPluginsUpdated`.
    pub async fn install_plugins(&self, sources: Vec<String>) -> QbtResult<()> {
        let (reply, rx) = oneshot::channel();
        self.inner.send(Op::Install { sources, reply })?;
        rx.await.map_err(|_| QbtError::other("search engine is shutting down"))?
    }

    pub fn uninstall_plugins(&self, names: &[String]) -> QbtResult<()> {
        let names: Vec<String> = names.iter().map(|n| n.trim().to_string()).filter(|n| is_valid_plugin_name(n)).collect();
        if names.is_empty() {
            return Ok(());
        }
        {
            let mut plugins = self.inner.plugins.write();
            for n in &names {
                plugins.remove(n);
            }
        }
        self.inner.send(Op::Uninstall { names })
    }

    pub fn enable_plugins(&self, names: &[String], enable: bool) -> QbtResult<()> {
        let mut changed = false;
        let mut persist = false;
        {
            let mut plugins = self.inner.plugins.write();
            let mut disabled = self.inner.disabled.lock();
            for name in names.iter().map(|n| n.trim()) {
                let Some(p) = plugins.get_mut(name) else { continue };
                changed |= p.enabled != enable;
                p.enabled = enable;
                persist |= if enable { disabled.remove(name) } else { disabled.insert(name.to_string()) };
            }
        }
        if persist {
            self.inner.send(Op::SaveConfig)?;
        }
        if changed {
            self.inner.emit_plugins(String::new());
        }
        Ok(())
    }

    /// Checks the official index and installs every plugin that is newer
    /// there or not installed yet (qBittorrent's "Check for updates").
    pub async fn update_plugins(&self) -> QbtResult<()> {
        let (reply, rx) = oneshot::channel();
        self.inner.send(Op::Update { reply })?;
        rx.await.map_err(|_| QbtError::other("search engine is shutting down"))?
    }

    // ---- Search jobs -------------------------------------------------------------

    /// `plugins`: names, or a single "all" / "enabled" ("multi"). Returns the
    /// new job id.
    pub fn start(&self, pattern: &str, plugins: &[String], category: &str) -> QbtResult<i64> {
        let inner = &self.inner;
        let pattern = sanitize_pattern(pattern)?;
        let category = normalize_category(category)?;
        let Some(python) = inner.usable_python() else {
            inner.maybe_redetect();
            return Err(QbtError::conflict(ERR_NO_PYTHON));
        };
        let plugins = inner.resolve_plugins(plugins)?;

        let job = {
            let mut jobs = inner.jobs.lock();
            if *inner.shutdown.borrow() {
                return Err(QbtError::other("search engine is shutting down"));
            }
            if jobs.iter().filter(|j| j.is_running()).count() >= MAX_RUNNING_JOBS {
                return Err(QbtError::conflict(format!("Unable to create more than {MAX_RUNNING_JOBS} concurrent searches.")));
            }
            while jobs.len() >= MAX_JOBS {
                match jobs.iter().position(|j| !j.is_running()) {
                    Some(i) => {
                        jobs.remove(i);
                    }
                    None => return Err(QbtError::conflict("Too many searches. Delete some finished searches first.")),
                }
            }
            let id = loop {
                let id = i64::from(rand::random_range(1..=i32::MAX));
                if !jobs.iter().any(|j| j.id == id) {
                    break id;
                }
            };
            let job = Arc::new(Job::new(id, pattern, category, plugins));
            jobs.push(job.clone());
            job
        };

        let launch = JobLaunch {
            python,
            nova_root: inner.dirs.root.clone(),
            nova2: inner.dirs.nova2(),
            env: inner.proxy_env(),
            engines_by_url: inner.engines_by_url(),
            events: inner.ctx.events.clone(),
            logger: inner.ctx.logger.clone(),
            timeout: inner.search_timeout,
        };
        let handle = inner.ctx.rt.spawn(job::run_job(job.clone(), launch));
        *job.task.lock() = Some(handle);
        Ok(job.id)
    }

    pub fn stop(&self, id: i64) -> QbtResult<()> {
        self.inner.job(id)?.request_stop();
        Ok(())
    }

    /// Status of one job, or of all jobs when `id` is `None` or 0.
    pub fn job_status(&self, id: Option<i64>) -> QbtResult<Vec<SearchJobInfo>> {
        match id {
            None | Some(0) => Ok(self.inner.jobs.lock().iter().map(|j| j.info()).collect()),
            Some(id) => Ok(vec![self.inner.job(id)?.info()]),
        }
    }

    /// `limit <= 0` = all; negative `offset` counts from the end.
    pub fn results(&self, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
        self.inner.job(id)?.page(limit, offset)
    }

    /// Stops the job and forgets it (its results are freed once the process
    /// has been reaped).
    pub fn delete(&self, id: i64) -> QbtResult<()> {
        let job = {
            let mut jobs = self.inner.jobs.lock();
            let i = jobs.iter().position(|j| j.id == id).ok_or_else(|| QbtError::not_found(format!("search job {id}")))?;
            jobs.remove(i)
        };
        job.request_stop();
        Ok(())
    }

    /// Adds a search result: magnet links directly; http(s) links through the
    /// plugin's `nova2dl.py` download routine (or directly when the plugin is
    /// not installed).
    pub async fn download_torrent(&self, url: &str, plugin: &str, params: AddTorrentParams) -> QbtResult<()> {
        let inner = &self.inner;
        let url = url.trim();
        let plugin = plugin.trim();
        if url.is_empty() || url.len() > MAX_TORRENT_URL_LEN || url.chars().any(char::is_control) {
            return Err(QbtError::invalid("Invalid torrent URL"));
        }
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("magnet:") {
            (inner.adder)(TorrentSource::Url { url: url.to_string() }, params).await?;
            return Ok(());
        }
        let is_http = lower.starts_with("http://") || lower.starts_with("https://");
        if !is_http || reqwest::Url::parse(url).is_err() {
            return Err(QbtError::invalid("Unsupported torrent URL; only magnet and http(s) links can be downloaded"));
        }
        let installed = is_valid_plugin_name(plugin) && inner.plugins.read().contains_key(plugin);
        if !installed {
            (inner.adder)(TorrentSource::Url { url: url.to_string() }, params).await?;
            return Ok(());
        }
        let python = inner.usable_python().ok_or_else(|| QbtError::conflict(ERR_NO_PYTHON))?;

        let nova2dl = inner.dirs.nova2dl();
        let cmd = python_command(&python, &inner.dirs.root, &inner.proxy_env(), [nova2dl.as_os_str(), OsStr::new(plugin), OsStr::new(url)]);
        let out = process::run(cmd, NOVA2DL_TIMEOUT, 64 * 1024, 64 * 1024).await?;
        let err = cap_text(&out.stderr_text());
        if !err.is_empty() {
            inner.ctx.logger.warning(format!(
                "Error occurred when downloading torrent via search engine. Engine: \"{plugin}\". URL: \"{url}\". Error: \"{err}\"."
            ));
        }
        if !out.success() {
            let why = if out.timed_out { "timed out".to_string() } else if err.is_empty() { "failed".to_string() } else { err };
            return Err(QbtError::network(format!("Downloading the torrent through the \"{plugin}\" search plugin {why}")));
        }
        let path = parse_download_output(&out.stdout)
            .ok_or_else(|| QbtError::other(format!("The \"{plugin}\" search plugin did not return a torrent file")))?;
        let limit = match inner.ctx.prefs.current().advanced.torrent_file_size_limit {
            l if l > 0 => l as u64,
            _ => 100 * 1024 * 1024,
        };
        let roots = allowed_temp_roots();
        let data = tokio::task::spawn_blocking(move || take_downloaded_file(&path, &roots, limit))
            .await
            .map_err(|e| QbtError::other(e.to_string()))??;
        (inner.adder)(TorrentSource::Bytes { data }, params).await?;
        Ok(())
    }
}

impl Inner {
    fn send(&self, op: Op) -> QbtResult<()> {
        self.ops.send(op).map_err(|_| QbtError::other("search engine is shutting down"))
    }

    fn emit_plugins(&self, message: String) {
        self.ctx.events.emit(CoreEvent::SearchPluginsUpdated { message });
    }

    fn job(&self, id: i64) -> QbtResult<Arc<Job>> {
        self.jobs
            .lock()
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(|| QbtError::not_found(format!("search job {id}")))
    }

    fn usable_python(&self) -> Option<PathBuf> {
        self.python.read().info.as_ref().filter(|i| i.is_supported()).map(|i| i.path.clone())
    }

    /// Queues a Python re-detection when none is usable (rate limited), so a
    /// Python installed while the app runs is picked up.
    fn maybe_redetect(&self) {
        let mut st = self.python.write();
        if st.info.as_ref().is_some_and(|i| i.is_supported())
            || st.redetect_pending
            || st.last_attempt.elapsed() < REDETECT_INTERVAL
        {
            return;
        }
        st.redetect_pending = true;
        let _ = self.ops.send(Op::DetectPython { quiet: false });
    }

    fn proxy_env(&self) -> ProxyEnv {
        proxy::proxy_env(&self.ctx.prefs.current().connection)
    }

    fn engines_by_url(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for p in self.plugins.read().values() {
            map.entry(p.url.clone()).or_insert_with(|| p.name.clone());
        }
        map
    }

    fn resolve_plugins(&self, selectors: &[String]) -> QbtResult<Vec<String>> {
        let sel: Vec<&str> = selectors.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let names: Vec<String> = if sel.len() == 1 {
            match sel[0].to_ascii_lowercase().as_str() {
                "all" => self.plugins.read().keys().cloned().collect(),
                "enabled" | "multi" => self.plugins.read().values().filter(|p| p.enabled).map(|p| p.name.clone()).collect(),
                _ => vec![sel[0].to_string()],
            }
        } else {
            sel.iter().map(|s| s.to_string()).collect()
        };
        if names.len() > MAX_PLUGINS_PER_JOB {
            return Err(QbtError::invalid("Too many search plugins selected"));
        }
        let mut out: Vec<String> = Vec::with_capacity(names.len());
        for n in names {
            if !is_valid_plugin_name(&n) {
                return Err(QbtError::invalid(format!("Invalid search plugin name: \"{n}\"")));
            }
            if !out.contains(&n) {
                out.push(n);
            }
        }
        Ok(out)
    }

    fn save_config(&self) {
        let cfg = SearchConfig { disabled_engines: self.disabled.lock().iter().cloned().collect() };
        let result = serde_json::to_vec_pretty(&cfg)
            .map_err(|e| QbtError::other(e.to_string()))
            .and_then(|data| write_atomic(&self.config_file, &data));
        if let Err(e) = result {
            self.ctx.logger.warning(format!("Failed to save search engine settings. File: \"{}\". Error: \"{e}\".", self.config_file.display()));
        }
    }

    async fn redetect(&self, quiet: bool) {
        let preferred = self.ctx.prefs.current().advanced.python_executable_path.clone();
        let info = python::detect(&preferred, &self.ctx.logger, quiet).await;
        let changed = {
            let mut st = self.python.write();
            let changed = st.info != info;
            st.info = info;
            st.last_attempt = Instant::now();
            st.redetect_pending = false;
            changed
        };
        if changed {
            self.refresh().await;
            self.emit_plugins(String::new());
        }
    }

    /// `SearchPluginManager::update()`: rebuilds the plugin list from
    /// `nova2.py --capabilities`. Keeps the current list when the run fails.
    /// Returns true when the list changed.
    async fn refresh(&self) -> bool {
        let Some(python) = self.usable_python() else { return false };
        let nova2 = self.dirs.nova2();
        let cmd = python_command(&python, &self.dirs.root, &self.proxy_env(), [nova2.as_os_str(), OsStr::new("--capabilities")]);
        let warn = |e: &str| {
            self.ctx.logger.warning(format!("Error occurred when fetching search engine capabilities. Error: \"{e}\"."));
        };
        let out = match process::run(cmd, CAPABILITIES_TIMEOUT, 4 * 1024 * 1024, 64 * 1024).await {
            Ok(o) => o,
            Err(e) => {
                warn(&e.to_string());
                return false;
            }
        };
        let err = cap_text(&out.stderr_text());
        if !err.is_empty() {
            warn(&err);
        }
        if out.timed_out {
            warn("timed out");
            return false;
        }
        let caps = match plugins::parse_capabilities(&String::from_utf8_lossy(&out.stdout)) {
            Ok(c) => c,
            Err(e) => {
                warn(&e);
                return false;
            }
        };

        let mut map = BTreeMap::new();
        for cap in caps {
            if !is_valid_plugin_name(&cap.name) {
                self.ctx.logger.warning(format!("Ignoring search plugin with an invalid name: \"{}\".", cap.name));
                continue;
            }
            let version = match nova::file_version_header(&self.dirs.plugin_file(&cap.name)) {
                VersionHeader::Valid(v) => v,
                VersionHeader::Invalid(s) => {
                    self.ctx.logger.warning(format!("Search plugin '{}.py' contains invalid version string ('{s}')", cap.name));
                    PluginVersion::default()
                }
                VersionHeader::Missing => PluginVersion::default(),
            };
            let entry = PluginEntry {
                enabled: true,
                icon_path: self.dirs.icon_file(&cap.name).map(|p| p.to_string_lossy().into_owned()),
                name: cap.name.clone(),
                full_name: cap.full_name,
                version,
                url: cap.url,
                categories: cap.categories,
            };
            map.insert(cap.name, entry);
        }
        // Enabled state and icons may have changed while nova2 ran: apply the
        // current values under the locks (order: plugins, then disabled).
        let mut current = self.plugins.write();
        {
            let disabled = self.disabled.lock();
            for e in map.values_mut() {
                e.enabled = !disabled.contains(&e.name);
                if e.icon_path.is_none() {
                    e.icon_path = current.get(&e.name).and_then(|old| old.icon_path.clone()).filter(|p| Path::new(p).is_file());
                }
            }
        }
        let changed = *current != map;
        *current = map;
        changed
    }

    async fn install_sources(&self, sources: Vec<String>) -> QbtResult<()> {
        let mut installed: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for source in sources.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match self.install_source(source).await {
                Ok(name) => installed.push(name),
                Err(f) => {
                    let what = if f.update { "update" } else { "install" };
                    failures.push(format!("Couldn't {what} \"{}\" search engine plugin. {}", f.name, f.reason));
                }
            }
        }
        let mut lines = Vec::new();
        if !installed.is_empty() {
            installed.sort_by_key(|n| n.to_lowercase());
            lines.push(format!("Plugins installed or updated: {}", installed.join(", ")));
        }
        lines.extend(failures.iter().cloned());
        if !lines.is_empty() {
            self.emit_plugins(lines.join("\n"));
        }
        if failures.is_empty() { Ok(()) } else { Err(QbtError::other(failures.join("\n"))) }
    }

    /// `SearchPluginManager::installPlugin()`.
    async fn install_source(&self, source: &str) -> Result<String, InstallFailure> {
        let fail = |name: String, reason: String| {
            let update = self.plugins.read().contains_key(&name);
            InstallFailure { name, update, reason }
        };
        match classify_source(source) {
            Source::Remote(url) => {
                let name = plugin_name_from_url(&url);
                if !is_valid_plugin_name(&name) {
                    return Err(fail(name, "Invalid plugin name.".into()));
                }
                let req = HttpRequest {
                    url: url.to_string(),
                    max_size: nova::MAX_PLUGIN_FILE_SIZE,
                    timeout_secs: 60,
                    ..Default::default()
                };
                match http::fetch(&self.ctx, req, HttpPurpose::General).await {
                    Ok(resp) if (200..300).contains(&resp.status) => self.install_data(&name, resp.body).await,
                    Ok(resp) => Err(fail(name, format!("Failed to download the plugin file. HTTP status {}", resp.status))),
                    Err(e) => Err(fail(name, format!("Failed to download the plugin file. {e}"))),
                }
            }
            Source::Local(path) => {
                let file_name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                if !file_name.to_ascii_lowercase().ends_with(".py") {
                    return Err(fail(file_name, "Unknown search engine plugin file format.".into()));
                }
                let name = file_name[..file_name.len() - 3].to_string();
                if !is_valid_plugin_name(&name) {
                    return Err(fail(name, "Invalid plugin name.".into()));
                }
                match read_local_plugin(&path) {
                    Ok(data) => self.install_data(&name, data).await,
                    Err(e) => Err(fail(name, e)),
                }
            }
            Source::Invalid(reason) => Err(fail(source.to_string(), reason)),
        }
    }

    /// `installPlugin_impl()`: validates the file, installs it atomically
    /// (keeping a backup of the previous version) and checks that nova2 can
    /// load it; rolls back otherwise.
    async fn install_data(&self, name: &str, data: Vec<u8>) -> Result<String, InstallFailure> {
        let installed = self.plugins.read().get(name).map(|p| p.version);
        let fail = |reason: &str| InstallFailure { name: name.to_string(), update: installed.is_some(), reason: reason.to_string() };
        let Some(python) = self.usable_python() else { return Err(fail(ERR_NO_PYTHON)) };
        if data.len() as u64 > nova::MAX_PLUGIN_FILE_SIZE {
            return Err(fail("The plugin file is too large."));
        }
        let new_version = match nova::parse_version_header(&data) {
            VersionHeader::Valid(v) => v,
            VersionHeader::Invalid(s) => {
                self.ctx.logger.warning(format!("Search plugin '{name}.py' contains invalid version string ('{s}')"));
                return Err(fail("The plugin has an invalid \"# VERSION:\" header."));
            }
            VersionHeader::Missing => return Err(fail("The plugin has no \"# VERSION: x.y\" header.")),
        };
        if let Some(old) = installed
            && old >= new_version
        {
            self.ctx.logger.info(format!("Plugin already at version {old}, which is greater than {new_version}"));
            return Err(fail("A more recent version of this plugin is already installed."));
        }

        // Stage the file next to its destination (a name nova2 never loads).
        let tmp = self.dirs.engines.join(format!(".install-{name}-{:08x}.tmp", rand::random::<u32>()));
        let staged = (|| -> std::io::Result<()> {
            use std::io::Write;
            std::fs::create_dir_all(&self.dirs.engines)?;
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            f.write_all(&data)?;
            f.sync_all()
        })();
        if let Err(e) = staged {
            let _ = std::fs::remove_file(&tmp);
            return Err(fail(&format!("Cannot write the plugin file: {e}")));
        }

        let cmd = python_command(&python, &self.dirs.root, &ProxyEnv::default(), [OsStr::new("-c"), OsStr::new(SYNTAX_CHECK), tmp.as_os_str()]);
        match process::run(cmd, SYNTAX_CHECK_TIMEOUT, 0, 16 * 1024).await {
            Ok(out) if out.success() => {}
            Ok(out) => {
                let _ = std::fs::remove_file(&tmp);
                let detail = out.stderr_text().lines().last().unwrap_or("").to_string();
                return Err(fail(&format!("The plugin is not valid Python code. {}", cap_text(&detail))));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(fail(&format!("Cannot run Python: {e}")));
            }
        }

        let dest = self.dirs.plugin_file(name);
        let backup = self.dirs.engines.join(format!("{name}.py.bak"));
        let updated = dest.symlink_metadata().is_ok();
        let moved = (|| -> std::io::Result<()> {
            if updated {
                std::fs::rename(&dest, &backup)?;
            }
            std::fs::rename(&tmp, &dest)
        })();
        if let Err(e) = moved {
            let _ = std::fs::remove_file(&tmp);
            if updated && !dest.exists() {
                let _ = std::fs::rename(&backup, &dest);
            }
            return Err(fail(&format!("Cannot install the plugin file: {e}")));
        }
        nova::clear_python_cache(&self.dirs.root);
        self.refresh().await;

        if !self.plugins.read().contains_key(name) {
            let _ = std::fs::remove_file(&dest);
            self.ctx.logger.info(format!("Plugin {name} is not supported."));
            if updated {
                let _ = std::fs::rename(&backup, &dest);
                self.refresh().await;
            }
            return Err(fail("Plugin is not supported."));
        }
        if updated {
            self.ctx.logger.info(format!("Plugin {name} has been successfully updated."));
            let _ = std::fs::remove_file(&backup);
        }
        Ok(name.to_string())
    }

    /// `checkForUpdates()` + `checkForUpdatesFinished()`.
    async fn update_all(&self) -> QbtResult<()> {
        let check_failed = |reason: String| {
            self.ctx.logger.info(format!("Failed to check for plugin updates: {reason}"));
            self.emit_plugins(format!("Sorry, couldn't check for plugin updates. {reason}"));
            Err(QbtError::network(reason))
        };
        let req = HttpRequest {
            url: format!("{}versions.txt", self.update_url),
            max_size: VERSIONS_TXT_MAX_SIZE,
            timeout_secs: 60,
            ..Default::default()
        };
        let body = match http::fetch(&self.ctx, req, HttpPurpose::General).await {
            Ok(r) if (200..300).contains(&r.status) => r.body,
            Ok(r) => return check_failed(format!("Update server is temporarily unavailable. HTTP status {}", r.status)),
            Err(e) => return check_failed(format!("Update server is temporarily unavailable. {e}")),
        };
        let index = plugins::parse_versions_txt(&body);
        if index.invalid > 0 {
            return check_failed(format!("Incorrect update info received for {} out of {} plugins.", index.invalid, index.total()));
        }
        let installed: HashMap<String, PluginVersion> = self.plugins.read().values().map(|p| (p.name.clone(), p.version)).collect();
        let mut needed = Vec::new();
        for (name, version) in index.entries {
            if installed.get(&name).is_none_or(|old| version > *old) {
                self.ctx.logger.info(format!("Plugin \"{name}\" is outdated, updating to version {version}"));
                needed.push(name);
            }
        }
        if needed.is_empty() {
            self.ctx.logger.info("All plugins are already up to date.");
            self.emit_plugins("All your plugins are already up to date.".into());
            return Ok(());
        }
        self.ctx.logger.info(format!("Updating {} plugins", needed.len()));
        let sources = needed
            .iter()
            .map(|name| {
                self.ctx.logger.info(format!("Updating plugin {name}"));
                format!("{}{name}.py", self.update_url)
            })
            .collect();
        self.install_sources(sources).await
    }

    /// `uninstallPlugin()`: deletes `engines/<name>.*` (script, icon, backup).
    fn uninstall_files(&self, names: &[String]) {
        nova::clear_python_cache(&self.dirs.root);
        let entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&self.dirs.engines).map(|d| d.flatten().collect()).unwrap_or_default();
        for name in names {
            let prefix = format!("{name}.");
            for e in &entries {
                let file_name = e.file_name();
                let Some(f) = file_name.to_str() else { continue };
                let is_file = e.file_type().map(|t| t.is_file() || t.is_symlink()).unwrap_or(false);
                if is_file && f.starts_with(&prefix) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            self.plugins.write().remove(name);
            self.icon_attempted.lock().remove(name);
        }
        self.emit_plugins(String::new());
    }

    /// Downloads `<plugin url>/favicon.ico` for plugins without an icon (once
    /// per plugin and session), like the Search plugins dialog does.
    fn schedule_icon_fetch(self: &Arc<Self>) {
        if !self.fetch_icons || *self.shutdown.borrow() {
            return;
        }
        let todo: Vec<(String, String)> = {
            let plugins = self.plugins.read();
            let mut attempted = self.icon_attempted.lock();
            plugins
                .values()
                .filter(|p| p.icon_path.is_none() && is_http_url(&p.url))
                .filter(|p| attempted.insert(p.name.clone()))
                .map(|p| (p.name.clone(), p.url.clone()))
                .collect()
        };
        if todo.is_empty() {
            return;
        }
        let inner = self.clone();
        self.ctx.rt.spawn(async move { inner.fetch_icons(todo).await });
    }

    async fn fetch_icons(&self, todo: Vec<(String, String)>) {
        let mut any = false;
        for (name, url) in todo {
            let req = HttpRequest {
                url: format!("{}/favicon.ico", url.trim_end_matches('/')),
                max_size: ICON_MAX_SIZE,
                timeout_secs: 30,
                ..Default::default()
            };
            let Ok(resp) = http::fetch(&self.ctx, req, HttpPurpose::General).await else { continue };
            if !(200..300).contains(&resp.status) {
                continue;
            }
            let Some(ext) = icon_extension(&resp.body) else { continue };
            if !self.plugins.read().contains_key(&name) {
                continue;
            }
            let path = self.dirs.engines.join(format!("{name}.{ext}"));
            if write_atomic(&path, &resp.body).is_ok()
                && let Some(p) = self.plugins.write().get_mut(&name)
            {
                p.icon_path = Some(path.to_string_lossy().into_owned());
                any = true;
            }
        }
        if any {
            self.emit_plugins(String::new());
        }
    }
}

async fn worker(inner: Arc<Inner>, mut rx: mpsc::UnboundedReceiver<Op>) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        let op = tokio::select! {
            op = rx.recv() => match op {
                Some(op) => op,
                None => break,
            },
            _ = shutdown.wait_for(|v| *v) => break,
        };
        match op {
            Op::Install { sources, reply } => {
                nova::clear_python_cache(&inner.dirs.root);
                let _ = reply.send(inner.install_sources(sources).await);
            }
            Op::Update { reply } => {
                nova::clear_python_cache(&inner.dirs.root);
                let _ = reply.send(inner.update_all().await);
            }
            Op::Uninstall { names } => inner.uninstall_files(&names),
            Op::SaveConfig => inner.save_config(),
            Op::DetectPython { quiet } => inner.redetect(quiet).await,
            Op::Refresh => {
                if inner.refresh().await {
                    inner.emit_plugins(String::new());
                }
            }
            #[cfg(test)]
            Op::Barrier { reply } => {
                let _ = reply.send(());
            }
        }
    }
}

/// Re-detects Python when the "Python executable path" preference changes.
async fn watch_prefs(inner: Arc<Inner>) {
    let mut rx = inner.ctx.prefs.subscribe();
    let mut shutdown = inner.shutdown.subscribe();
    let mut last = rx.borrow_and_update().advanced.python_executable_path.clone();
    loop {
        tokio::select! {
            r = rx.changed() => if r.is_err() { break },
            _ = shutdown.wait_for(|v| *v) => break,
        }
        let current = rx.borrow_and_update().advanced.python_executable_path.clone();
        if current != last {
            last = current;
            let _ = inner.ops.send(Op::DetectPython { quiet: false });
        }
    }
}

fn load_config(path: &Path, ctx: &CoreContext) -> BTreeSet<String> {
    match std::fs::read(path) {
        Ok(data) => match serde_json::from_slice::<SearchConfig>(&data) {
            Ok(cfg) => cfg.disabled_engines.into_iter().filter(|n| is_valid_plugin_name(n)).collect(),
            Err(e) => {
                ctx.logger.warning(format!("Failed to load search engine settings. File: \"{}\". Error: \"{e}\".", path.display()));
                BTreeSet::new()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(e) => {
            ctx.logger.warning(format!("Failed to load search engine settings. File: \"{}\". Error: \"{e}\".", path.display()));
            BTreeSet::new()
        }
    }
}

/// Trims the pattern and replaces control characters (NUL included) with
/// spaces; rejects empty or overly long patterns.
fn sanitize_pattern(pattern: &str) -> QbtResult<String> {
    let cleaned: String = pattern.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return Err(QbtError::invalid("Please type a search pattern first"));
    }
    if trimmed.chars().count() > MAX_PATTERN_CHARS {
        return Err(QbtError::invalid("The search pattern is too long"));
    }
    Ok(trimmed.to_string())
}

/// Lower-cases the category; "" means "all"; only nova2's categories pass.
fn normalize_category(category: &str) -> QbtResult<String> {
    let c = category.trim().to_ascii_lowercase();
    if c.is_empty() {
        return Ok("all".into());
    }
    if plugins::CATEGORIES.contains(&c.as_str()) {
        Ok(c)
    } else {
        Err(QbtError::invalid(format!("Invalid search category: \"{}\"", category.trim())))
    }
}

fn is_http_url(s: &str) -> bool {
    reqwest::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

enum Source {
    Remote(reqwest::Url),
    Local(PathBuf),
    Invalid(String),
}

/// http(s) URLs are downloaded, `file:` URLs and absolute paths are local;
/// any other scheme is rejected.
fn classify_source(source: &str) -> Source {
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return match reqwest::Url::parse(source) {
            Ok(u) if u.host_str().is_some() => Source::Remote(u),
            _ => Source::Invalid("Invalid plugin URL.".into()),
        };
    }
    if lower.starts_with("file:") {
        return match reqwest::Url::parse(source).ok().and_then(|u| u.to_file_path().ok()) {
            Some(p) => Source::Local(p),
            None => Source::Invalid("Invalid plugin file URL.".into()),
        };
    }
    let path = Path::new(source);
    if path.is_absolute() {
        return Source::Local(path.to_path_buf());
    }
    let has_scheme = source
        .split_once(':')
        .is_some_and(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    if has_scheme {
        Source::Invalid("Unsupported plugin source; use an http(s) URL or a local .py file.".into())
    } else {
        Source::Invalid("The plugin file path must be absolute.".into())
    }
}

/// Plugin id from a download URL: last path segment without its extension.
fn plugin_name_from_url(url: &reqwest::Url) -> String {
    let seg = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    Path::new(seg).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn read_local_plugin(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Cannot read the plugin file: {e}"))?;
    if !meta.is_file() {
        return Err("The plugin path is not a file.".into());
    }
    if meta.len() > nova::MAX_PLUGIN_FILE_SIZE {
        return Err("The plugin file is too large.".into());
    }
    let mut data = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(nova::MAX_PLUGIN_FILE_SIZE + 1).read_to_end(&mut data))
        .map_err(|e| format!("Cannot read the plugin file: {e}"))?;
    Ok(data)
}

/// Favicon type by magic bytes (only formats the GUI can show are kept).
fn icon_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.len() > 6 && data.starts_with(&[0, 0, 1, 0]) && (data[4] != 0 || data[5] != 0) {
        Some("ico")
    } else {
        None
    }
}

/// `nova2dl.py` prints `"<file path> <url>"`; returns the path.
fn parse_download_output(stdout: &[u8]) -> Option<PathBuf> {
    let text = String::from_utf8_lossy(stdout);
    let line = text.lines().map(str::trim).rfind(|l| !l.is_empty())?;
    let (path, _url) = line.rsplit_once(' ')?;
    let path = path.trim();
    (!path.is_empty() && Path::new(path).is_absolute()).then(|| PathBuf::from(path))
}

/// Where `tempfile.mkstemp()` may put downloads (canonical paths).
fn allowed_temp_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for p in [std::env::temp_dir(), PathBuf::from("/tmp"), PathBuf::from("/var/tmp")] {
        if let Ok(c) = std::fs::canonicalize(&p)
            && !roots.contains(&c)
        {
            roots.push(c);
        }
    }
    roots
}

/// Reads and deletes the file `nova2dl.py` produced. The path must be a
/// regular file (not a symlink) inside one of `roots`; it is opened with
/// `O_NOFOLLOW` and read through the descriptor, so it cannot be swapped
/// between the check and the read. Files failing the checks are left alone.
fn take_downloaded_file(path: &Path, roots: &[PathBuf], limit: u64) -> QbtResult<Vec<u8>> {
    use std::os::unix::fs::OpenOptionsExt;
    let invalid = |why: &str| QbtError::other(format!("Rejected the file returned by the search plugin ({why}): {}", path.display()));
    if !path.is_absolute() {
        return Err(invalid("not an absolute path"));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|_| invalid("missing"))?;
    if !meta.file_type().is_file() {
        return Err(invalid("not a regular file"));
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| invalid("missing"))?;
    if !roots.iter().any(|r| canonical.starts_with(r) && canonical != *r) {
        return Err(invalid("outside the temporary folder"));
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&canonical)
        .map_err(|_| invalid("cannot open"))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(invalid("not a regular file"));
    }
    if meta.len() > limit {
        let _ = std::fs::remove_file(&canonical);
        return Err(QbtError::invalid(format!("The torrent file exceeds the size limit of {limit} bytes")));
    }
    let mut data = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1).read_to_end(&mut data)?;
    let _ = std::fs::remove_file(&canonical);
    if data.len() as u64 > limit {
        return Err(QbtError::invalid(format!("The torrent file exceeds the size limit of {limit} bytes")));
    }
    Ok(data)
}
