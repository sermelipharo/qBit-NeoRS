//! Starts and stops the plugins thread with the global switch and forwards
//! the facade's requests to it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use tokio::sync::oneshot;

use super::host::PluginHost;
use super::runtime::Limits;
use super::worker::{self, WorkerConfig};
use super::{EVENT_QUEUE_CAPACITY, Job, PluginInstallResult, PluginsStatus};
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;

/// Requests handled by the plugins thread.
pub(crate) enum Control {
    Install { path: PathBuf, reply: oneshot::Sender<QbtResult<PluginInstallResult>> },
    Uninstall { id: String, reply: oneshot::Sender<QbtResult<()>> },
    SetEnabled { id: String, enabled: bool, reply: oneshot::Sender<QbtResult<()>> },
    Invoke { id: String, reply: oneshot::Sender<QbtResult<()>> },
    Shutdown,
}

struct Running {
    tx: SyncSender<Job>,
    stopping: Arc<AtomicBool>,
    done: Receiver<()>,
}

pub struct PluginsManager {
    ctx: CoreContext,
    host: Arc<dyn PluginHost>,
    limits: Limits,
    status: Arc<RwLock<PluginsStatus>>,
    running: Mutex<Option<Running>>,
}

fn off() -> QbtError {
    QbtError::conflict("Plugins are turned off.")
}

impl PluginsManager {
    /// Creates the manager, starts the engine when the switch is on and
    /// follows the switch afterwards.
    pub fn launch(ctx: &CoreContext, host: Arc<dyn PluginHost>) -> Arc<Self> {
        Self::launch_with(ctx, host, Limits::default())
    }

    pub(crate) fn launch_with(ctx: &CoreContext, host: Arc<dyn PluginHost>, limits: Limits) -> Arc<Self> {
        let me = Arc::new(Self {
            ctx: ctx.clone(),
            host,
            limits,
            status: Arc::new(RwLock::new(PluginsStatus {
                folder: Self::folder(ctx).to_string_lossy().into_owned(),
                ..Default::default()
            })),
            running: Mutex::new(None),
        });
        let enabled = ctx.prefs.current().advanced.plugins_enabled;
        if enabled {
            me.start();
        }
        let weak: Weak<Self> = Arc::downgrade(&me);
        let mut rx = ctx.prefs.subscribe();
        ctx.rt.spawn(async move {
            let mut last = enabled;
            while rx.changed().await.is_ok() {
                let now = rx.borrow_and_update().advanced.plugins_enabled;
                if now == last {
                    continue;
                }
                last = now;
                let Some(me) = weak.upgrade() else { break };
                // Starting and stopping touch the disk and join a thread.
                let _ = tokio::task::spawn_blocking(move || if now { me.start() } else { me.stop() }).await;
            }
        });
        me
    }

    fn folder(ctx: &CoreContext) -> PathBuf {
        ctx.profile.data_dir.join("plugins")
    }

    /// Starts the plugins thread (no-op when running).
    fn start(&self) {
        let mut running = self.running.lock();
        if running.is_some() {
            return;
        }
        let (tx, rx) = sync_channel::<Job>(EVENT_QUEUE_CAPACITY);
        let (done_tx, done_rx) = sync_channel::<()>(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let cfg = WorkerConfig {
            plugins_dir: Self::folder(&self.ctx),
            config_file: self.ctx.profile.config_dir.join("plugins.json"),
            host: self.host.clone(),
            logger: self.ctx.logger.clone(),
            events: self.ctx.events.clone(),
            rt: self.ctx.rt.clone(),
            limits: self.limits,
            status: self.status.clone(),
            bus: self.ctx.plugins.clone(),
            stopping: stopping.clone(),
        };
        let spawned = std::thread::Builder::new().name("qbt-plugins".into()).spawn(move || {
            worker::run(cfg, rx);
            let _ = done_tx.send(());
        });
        if let Err(e) = spawned {
            self.ctx.logger.warning(format!("Couldn't start the plugins engine. Reason: {e}"));
            return;
        }
        self.ctx.logger.info("Plugins engine started.");
        self.ctx.plugins.connect(Some(tx.clone()));
        *running = Some(Running { tx, stopping, done: done_rx });
    }

    /// Stops the plugins thread: no new events, pending ones are skipped,
    /// every Lua state is closed. Waits up to 3 s for the thread.
    pub fn stop(&self) {
        let Some(r) = self.running.lock().take() else { return };
        self.ctx.plugins.connect(None);
        r.stopping.store(true, Ordering::Relaxed);
        let _ = r.tx.send(Job::Control(Control::Shutdown));
        if r.done.recv_timeout(Duration::from_secs(3)).is_err() {
            self.ctx.logger.warning("The plugins engine did not stop in time; it stops when its current call ends.");
        }
        {
            let mut s = self.status.write();
            s.running = false;
            s.plugins.clear();
            s.problems.clear();
        }
        self.ctx.logger.info("Plugins engine stopped.");
        self.ctx.events.emit(CoreEvent::PluginsChanged);
    }

    pub fn status(&self) -> PluginsStatus {
        self.status.read().clone()
    }

    async fn request<T: Send + 'static>(&self, make: impl FnOnce(oneshot::Sender<QbtResult<T>>) -> Control) -> QbtResult<T> {
        let tx = self.running.lock().as_ref().map(|r| r.tx.clone()).ok_or_else(off)?;
        let (reply, rx) = oneshot::channel();
        let job = Job::Control(make(reply));
        tokio::task::spawn_blocking(move || tx.send(job))
            .await
            .map_err(|e| QbtError::other(e.to_string()))?
            .map_err(|_| off())?;
        rx.await.map_err(|_| off())?
    }

    /// Installs or updates a plugin from a local `.lua` file.
    pub async fn install(&self, path: String) -> QbtResult<PluginInstallResult> {
        self.request(|reply| Control::Install { path: PathBuf::from(path), reply }).await
    }

    pub async fn uninstall(&self, id: String) -> QbtResult<()> {
        self.request(|reply| Control::Uninstall { id, reply }).await
    }

    pub async fn set_enabled(&self, id: String, enabled: bool) -> QbtResult<()> {
        self.request(|reply| Control::SetEnabled { id, enabled, reply }).await
    }

    /// Calls the plugin's `invoke` function (the Plugins menu).
    pub async fn invoke(&self, id: String) -> QbtResult<()> {
        self.request(|reply| Control::Invoke { id, reply }).await
    }
}
