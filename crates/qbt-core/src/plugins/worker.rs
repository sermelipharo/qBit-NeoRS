//! The plugins thread: owns every Lua state, loads plugins, handles
//! installs/uninstalls/toggles and calls event handlers, one at a time.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use mlua::{Function, Lua, Value};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::api::{self, PluginCtx, Stats};
use super::fs::{self as pfs, PLUGIN_EXTENSION};
use super::host::PluginHost;
use super::manager::Control;
use super::runtime::{self, Budget, Limits, Metadata};
use super::{Job, PluginArg, PluginBus, PluginHandler, PluginInfo, PluginInstallResult, PluginsStatus};
use crate::error::{QbtError, QbtResult};
use crate::events::{CoreEvent, EventBus};
use crate::logger::Logger;

/// A plugin that keeps exhausting its budget is disabled after this many
/// calls in a row.
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

/// Everything the worker needs from the app.
pub(crate) struct WorkerConfig {
    pub plugins_dir: PathBuf,
    pub config_file: PathBuf,
    pub host: Arc<dyn PluginHost>,
    pub logger: Arc<Logger>,
    pub events: EventBus,
    pub rt: tokio::runtime::Handle,
    pub limits: Limits,
    pub status: Arc<RwLock<PluginsStatus>>,
    pub bus: PluginBus,
    pub stopping: Arc<AtomicBool>,
}

struct Plugin {
    id: String,
    lua: Lua,
    budget: Rc<Budget>,
    stats: Rc<Stats>,
    meta: Metadata,
    invocable: bool,
    handlers: u32,
    enabled: bool,
    path: PathBuf,
    size: i64,
    sha256: String,
    failures: i64,
    last_error: String,
    consecutive_timeouts: u32,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy)]
struct EntryConfig {
    #[serde(default)]
    enabled: bool,
}

struct Worker {
    cfg: WorkerConfig,
    plugins: BTreeMap<String, Plugin>,
    problems: Vec<String>,
    folder_ok: bool,
}

pub(crate) fn run(cfg: WorkerConfig, rx: Receiver<Job>) {
    let mut w = Worker { cfg, plugins: BTreeMap::new(), problems: Vec::new(), folder_ok: false };
    w.load_all();
    w.publish();
    while let Ok(job) = rx.recv() {
        match job {
            Job::Event { handler, args } => {
                if !w.cfg.stopping.load(Ordering::Relaxed) {
                    w.dispatch(handler, args);
                }
            }
            Job::Control(Control::Shutdown) => break,
            Job::Control(c) => w.control(c),
        }
    }
    // Lua states close here, on the thread that created them.
    drop(w.plugins);
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

impl Worker {
    fn log_warning(&self, msg: String) {
        self.cfg.logger.warning(msg);
    }

    // ---- Loading ------------------------------------------------------------

    fn load_all(&mut self) {
        if let Err(e) = pfs::prepare_folder(&self.cfg.plugins_dir) {
            self.log_warning(format!("Couldn't load plugins. Reason: {e}"));
            self.problems.push(e);
            return;
        }
        self.folder_ok = true;
        let files = match pfs::list_plugin_files(&self.cfg.plugins_dir) {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("Couldn't read folder '{}'. {e}", self.cfg.plugins_dir.display());
                self.log_warning(format!("Couldn't load plugins. Reason: {msg}"));
                self.problems.push(msg);
                return;
            }
        };
        for (id, path) in files {
            let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match pfs::read_installed(&path).and_then(|data| self.load_plugin(&id, &path, &data)) {
                Ok(p) => {
                    self.cfg.logger.info(format!("Loaded plugin. ID: {id}."));
                    self.plugins.insert(id, p);
                }
                Err(reason) => {
                    self.log_warning(format!("Couldn't load plugin. File: {file_name}. Reason: {reason}"));
                    self.problems.push(format!("{file_name}: {reason}"));
                }
            }
        }
        if !self.plugins.is_empty() {
            let config = self.read_config();
            for (id, p) in &mut self.plugins {
                p.enabled = config.get(id).is_some_and(|c| c.enabled);
            }
        }
    }

    /// `Plugin::load` + the API registration of `Plugin::Plugin`.
    fn load_plugin(&self, id: &str, path: &Path, data: &[u8]) -> Result<Plugin, String> {
        let chunk_name = format!("{id}.{PLUGIN_EXTENSION}");
        let (lua, budget, meta) = runtime::load(data, &chunk_name, &self.cfg.limits)?;
        let invocable = runtime::has_function(&lua, "invoke");
        let handlers = PluginHandler::ALL.iter().filter(|h| runtime::has_function(&lua, h.name())).fold(0, |m, h| m | h.bit());
        let stats = Rc::new(Stats::default());
        lua.set_app_data(PluginCtx {
            id: id.to_string(),
            host: self.cfg.host.clone(),
            logger: self.cfg.logger.clone(),
            rt: self.cfg.rt.clone(),
            budget: budget.clone(),
            stats: stats.clone(),
        });
        api::register(&lua).map_err(|e| runtime::error_message(&e))?;
        Ok(Plugin {
            id: id.to_string(),
            lua,
            budget,
            stats,
            meta,
            invocable,
            handlers,
            enabled: false,
            path: path.to_path_buf(),
            size: data.len() as i64,
            sha256: sha256_hex(data),
            failures: 0,
            last_error: String::new(),
            consecutive_timeouts: 0,
        })
    }

    // ---- Configuration (config/plugins.json) ------------------------------------------

    fn read_config(&self) -> BTreeMap<String, EntryConfig> {
        let path = &self.cfg.config_file;
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
            Err(e) => {
                self.log_warning(format!("Failed to load plugins configuration. {e}"));
                return BTreeMap::new();
            }
        };
        match serde_json::from_slice::<serde_json::Value>(&data) {
            Ok(serde_json::Value::Object(map)) => map
                .into_iter()
                .map(|(k, v)| (k, serde_json::from_value::<EntryConfig>(v).unwrap_or_default()))
                .collect(),
            Ok(_) => {
                self.log_warning(format!(
                    "Failed to load plugins configuration from {}. Error: \"Invalid data format.\"",
                    path.display()
                ));
                BTreeMap::new()
            }
            Err(e) => {
                self.log_warning(format!("Failed to parse plugins configuration from {}. Error: \"{e}\"", path.display()));
                BTreeMap::new()
            }
        }
    }

    /// Writes the enabled flags of the loaded plugins, keeping the entries
    /// of plugin files that exist but did not load (so a permission problem
    /// does not lose their state).
    fn store_config(&self) {
        let mut config = self.read_config();
        config.retain(|id, _| self.plugins.contains_key(id) || self.cfg.plugins_dir.join(format!("{id}.{PLUGIN_EXTENSION}")).exists());
        for (id, p) in &self.plugins {
            config.insert(id.clone(), EntryConfig { enabled: p.enabled });
        }
        let data = serde_json::to_vec_pretty(&config).unwrap_or_default();
        if let Err(e) = crate::utils::fs::write_atomic(&self.cfg.config_file, &data) {
            self.log_warning(format!("Couldn't store plugins configuration to {}. Error: {e}", self.cfg.config_file.display()));
        }
    }

    // ---- Status ---------------------------------------------------------------

    fn info(p: &Plugin) -> PluginInfo {
        PluginInfo {
            id: p.id.clone(),
            name: p.meta.name.clone(),
            version: p.meta.version.to_string(),
            author: p.meta.author.clone(),
            description: p.meta.description.clone(),
            invocable: p.invocable,
            enabled: p.enabled,
            handlers: PluginHandler::ALL.iter().filter(|h| p.handlers & h.bit() != 0).map(|h| h.name().to_string()).collect(),
            file_path: p.path.to_string_lossy().into_owned(),
            file_size: p.size,
            sha256: p.sha256.clone(),
            programs_run: p.stats.programs_run.get(),
            mails_sent: p.stats.mails_sent.get(),
            failures: p.failures,
            last_error: p.last_error.clone(),
        }
    }

    /// Publishes the plugin list and the handler mask.
    fn publish(&self) {
        let mask = self.plugins.values().filter(|p| p.enabled).fold(0, |m, p| m | p.handlers);
        if !self.cfg.stopping.load(Ordering::Relaxed) {
            self.cfg.bus.set_mask(mask);
        }
        {
            let mut s = self.cfg.status.write();
            s.running = true;
            s.folder = self.cfg.plugins_dir.to_string_lossy().into_owned();
            s.plugins = self.plugins.values().map(Self::info).collect();
            s.problems = self.problems.clone();
        }
        self.cfg.events.emit(CoreEvent::PluginsChanged);
    }

    // ---- Calls ------------------------------------------------------------------

    fn dispatch(&mut self, handler: PluginHandler, args: Vec<PluginArg>) {
        let ids: Vec<String> =
            self.plugins.values().filter(|p| p.enabled && p.handlers & handler.bit() != 0).map(|p| p.id.clone()).collect();
        let mut changed = false;
        for id in ids {
            changed |= self.call(&id, handler.name(), args.clone());
        }
        if changed {
            self.publish();
        }
    }

    /// Calls a global function of a plugin within its budget. Returns true
    /// when the plugin's shown state changed (failure, auto-disable, counters).
    fn call(&mut self, id: &str, function: &str, args: Vec<PluginArg>) -> bool {
        let limits = self.cfg.limits;
        let Some(p) = self.plugins.get_mut(id) else { return false };
        let before = (p.stats.programs_run.get(), p.stats.mails_sent.get());
        let f = match p.lua.globals().get::<Value>(function) {
            Ok(Value::Function(f)) => f,
            _ => return false,
        };
        let result = call_function(&p.lua, &p.budget, &limits, &f, args);
        let mut changed = before != (p.stats.programs_run.get(), p.stats.mails_sent.get());
        match result {
            Ok(()) => p.consecutive_timeouts = 0,
            Err(reason) => {
                changed = true;
                p.failures += 1;
                p.last_error = reason.clone();
                let logger = self.cfg.logger.clone();
                logger.warning(format!("Failed to call the plugin. Plugin: {id}. Reason: {reason}"));
                if p.budget.exhausted() {
                    p.consecutive_timeouts += 1;
                    if p.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS && p.enabled {
                        p.enabled = false;
                        logger.warning(format!(
                            "Plugin '{id}' was disabled because it ran out of time {MAX_CONSECUTIVE_TIMEOUTS} times in a row."
                        ));
                        self.store_config();
                    }
                } else {
                    p.consecutive_timeouts = 0;
                }
            }
        }
        changed
    }

    // ---- Control --------------------------------------------------------------------

    fn control(&mut self, c: Control) {
        match c {
            Control::Install { path, reply } => {
                let r = self.install(&path);
                if r.is_ok() {
                    self.publish();
                }
                let _ = reply.send(r);
            }
            Control::Uninstall { id, reply } => {
                let r = self.uninstall(&id);
                self.publish();
                let _ = reply.send(r);
            }
            Control::SetEnabled { id, enabled, reply } => {
                let r = self.set_enabled(&id, enabled);
                self.publish();
                let _ = reply.send(r);
            }
            Control::Invoke { id, reply } => {
                let r = self.invoke(&id);
                self.publish();
                let _ = reply.send(r);
            }
            Control::Shutdown => {}
        }
    }

    /// `PluginsEngine::installPluginImpl`: a new plugin is installed
    /// disabled; an update must have a higher version and keeps the
    /// enabled flag. Failures of an update are `Conflict` errors, those of
    /// a new install `InvalidArgument` errors (the window words them
    /// differently).
    fn install(&mut self, source: &Path) -> QbtResult<PluginInstallResult> {
        let shown = source.display().to_string();
        let fail = |this: &Self, existing: bool, reason: String| -> QbtError {
            if existing {
                this.log_warning(format!("Couldn't update plugin. File: {shown}. Reason: {reason}"));
                QbtError::conflict(reason)
            } else {
                this.log_warning(format!("Couldn't install plugin. File: {shown}. Reason: {reason}"));
                QbtError::invalid(reason)
            }
        };
        if !source.is_absolute() {
            return Err(fail(self, false, format!("'{shown}' is invalid plugin path. An absolute path is expected.")));
        }
        let file_name = source.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(id) = pfs::plugin_id(&file_name) else {
            return Err(fail(self, false, format!("'{file_name}' is not a plugin file name (expected <name>.lua).")));
        };
        if !self.folder_ok {
            return Err(fail(self, false, self.problems.first().cloned().unwrap_or_else(|| "The plugins folder is not usable.".into())));
        }
        let existing = self.plugins.contains_key(&id);
        let data = pfs::read_source(source).map_err(|e| fail(self, existing, e))?;
        let target = self.cfg.plugins_dir.join(&file_name);
        let mut plugin = self.load_plugin(&id, &target, &data).map_err(|e| fail(self, existing, e))?;
        let previous = self.plugins.get(&id).map(|p| (p.meta.version, p.enabled));
        if let Some((old, _)) = previous {
            if plugin.meta.version == old {
                return Err(fail(self, true, format!("The same version of plugin '{id}' is already installed.")));
            }
            if plugin.meta.version < old {
                return Err(fail(self, true, format!("The newest version of plugin '{id}' is already installed.")));
            }
        }
        if let Err(e) = pfs::write_installed(&target, &data) {
            return Err(fail(self, existing, format!("Couldn't copy file '{shown}' to plugins directory. {e}")));
        }
        plugin.enabled = previous.is_some_and(|(_, enabled)| enabled);
        let version = plugin.meta.version.to_string();
        self.plugins.insert(id.clone(), plugin);
        self.problems.retain(|p| !p.starts_with(&format!("{file_name}: ")));
        if previous.is_some() {
            self.cfg.logger.info(format!("Updated plugin. ID: {id}. Version: {version}."));
        } else {
            self.cfg.logger.info(format!("Installed plugin. ID: {id}. Version: {version}."));
        }
        self.store_config();
        Ok(PluginInstallResult {
            plugin: Self::info(&self.plugins[&id]),
            updated: previous.is_some(),
            previous_version: previous.map(|(v, _)| v.to_string()).unwrap_or_default(),
        })
    }

    fn uninstall(&mut self, id: &str) -> QbtResult<()> {
        if !self.plugins.contains_key(id) {
            let reason = format!("Plugin '{id}' is not installed.");
            self.log_warning(format!("Couldn't uninstall plugin. ID: {id}. Reason: {reason}"));
            return Err(QbtError::not_found(reason));
        }
        let path = self.cfg.plugins_dir.join(format!("{id}.{PLUGIN_EXTENSION}"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                let reason = e.to_string();
                self.log_warning(format!("Couldn't uninstall plugin. ID: {id}. Reason: {reason}"));
                // As in the Plugins dialog: disable it instead.
                if let Some(p) = self.plugins.get_mut(id) {
                    p.enabled = false;
                }
                self.store_config();
                return Err(QbtError::io(reason));
            }
        }
        self.plugins.remove(id);
        self.cfg.logger.info(format!("Uninstalled plugin. ID: {id}."));
        self.store_config();
        Ok(())
    }

    fn set_enabled(&mut self, id: &str, enabled: bool) -> QbtResult<()> {
        let p = self.plugins.get_mut(id).ok_or_else(|| QbtError::not_found(format!("Plugin '{id}' is not installed.")))?;
        if p.enabled != enabled {
            p.enabled = enabled;
            p.consecutive_timeouts = 0;
            self.cfg.logger.info(if enabled { format!("Enabled plugin. ID: {id}.") } else { format!("Disabled plugin. ID: {id}.") });
            self.store_config();
        }
        Ok(())
    }

    fn invoke(&mut self, id: &str) -> QbtResult<()> {
        let p = self.plugins.get(id).ok_or_else(|| QbtError::not_found(format!("Plugin '{id}' is not installed.")))?;
        if !p.enabled {
            return Err(QbtError::conflict(format!("Plugin '{id}' is disabled.")));
        }
        if !p.invocable {
            return Err(QbtError::conflict(format!("Plugin '{id}' can't be invoked.")));
        }
        let failures = p.failures;
        self.cfg.logger.info(format!("Invoking plugin. ID: {id}."));
        self.call(id, "invoke", Vec::new());
        match self.plugins.get(id) {
            Some(p) if p.failures > failures => Err(QbtError::other(p.last_error.clone())),
            _ => Ok(()),
        }
    }
}

/// Calls `f` with the plugin's arguments within the budget.
fn call_function(lua: &Lua, budget: &Budget, limits: &Limits, f: &Function, args: Vec<PluginArg>) -> Result<(), String> {
    budget.start(limits);
    let args = api::to_lua_args(lua, args).map_err(|e| runtime::error_message(&e))?;
    f.call::<()>(args).map_err(|e| runtime::error_message(&e))
}
