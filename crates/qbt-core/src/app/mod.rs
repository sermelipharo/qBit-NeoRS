//! The application facade: one object that owns the async runtime and every
//! subsystem, and exposes the complete operation set used by all front ends
//! (SwiftUI app through UniFFI, WebAPI, headless daemon).
//!
//! The facade methods are split by area into `api_*.rs` files, each a separate
//! `impl App` block, so subsystems can evolve independently. Rules:
//! * Methods that only read/modify in-memory state are synchronous and must
//!   never block on I/O.
//! * Anything that touches the engine, disk or network is `async` and runs its
//!   work on the app runtime via [`App::spawn_on_rt`] (UniFFI polls futures on
//!   foreign threads that have no Tokio context).

mod api_app;
mod api_creator;
mod api_gui;
mod api_log;
mod api_names;
mod api_rss;
mod api_search;
mod api_torrents;
mod api_transfer;
mod config;

use std::sync::Arc;

pub use config::{AppConfig, Profile};
pub(crate) use api_names::NameRulesSnapshot;

use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::{EventBus, EventListener};
use crate::logger::Logger;
use crate::prefs::PreferencesStore;
use crate::{bittorrent, creator, rss, search, tracker, webui};

#[cfg_attr(feature = "ffi", derive(uniffi::Object))]
pub struct App {
    pub(crate) rt: tokio::runtime::Runtime,
    pub(crate) ctx: CoreContext,
    pub(crate) session: Arc<bittorrent::Session>,
    pub(crate) rss: Arc<rss::RssManager>,
    pub(crate) search: Arc<search::SearchManager>,
    pub(crate) creator: Arc<creator::CreatorManager>,
    pub(crate) webui: Arc<webui::WebUiController>,
    pub(crate) tracker: Arc<tracker::EmbeddedTracker>,
    /// Kept alive for the lifetime of the app; reacts to preference changes.
    #[allow(dead_code)]
    pub(crate) dyndns: Arc<crate::net::dyndns::DynDnsUpdater>,
    pub(crate) gui_sync: parking_lot::Mutex<bittorrent::sync::GuiSyncState>,
    /// Torrent name rules pushed by the macOS app (memory only).
    pub(crate) name_rules: parking_lot::RwLock<Option<Arc<NameRulesSnapshot>>>,
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// Creates the runtime, loads the profile and starts every subsystem.
    #[cfg_attr(feature = "ffi", uniffi::constructor)]
    pub fn new(config: AppConfig) -> QbtResult<Arc<Self>> {
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(2, 4);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .max_blocking_threads(16)
            .thread_name("qbt-core")
            .enable_all()
            .build()
            .map_err(|e| QbtError::other(format!("cannot start runtime: {e}")))?;

        let profile = Arc::new(Profile::resolve(&config));
        for dir in [&profile.config_dir, &profile.data_dir, &profile.cache_dir] {
            std::fs::create_dir_all(dir)?;
        }
        let events = EventBus::default();
        let prefs = Arc::new(PreferencesStore::load(&profile)?);
        let logger = Arc::new(Logger::new(&profile, &prefs, events.clone(), config.log_to_stderr));
        let ctx = CoreContext {
            rt: rt.handle().clone(),
            profile,
            prefs,
            events,
            logger,
            config: Arc::new(config),
        };

        let (session, rss, search, creator, webui) = rt.block_on(async {
            let session = bittorrent::Session::launch(&ctx).await?;
            let rss = rss::RssManager::launch(&ctx, session.clone()).await?;
            let search = search::SearchManager::launch(&ctx, session.clone()).await?;
            let creator = creator::CreatorManager::launch(&ctx, session.clone()).await?;
            QbtResult::Ok((session, rss, search, creator))
        })
        .map(|(s, r, se, c)| {
            let webui = webui::WebUiController::new(&ctx);
            (s, r, se, c, webui)
        })?;

        let tracker = {
            let _guard = rt.enter();
            tracker::EmbeddedTracker::launch(&ctx)
        };
        let dyndns = {
            let _guard = rt.enter();
            crate::net::dyndns::launch(&ctx)
        };

        let app = Arc::new(Self {
            rt,
            ctx,
            tracker,
            dyndns,
            session,
            rss,
            search,
            creator,
            webui,
            gui_sync: Default::default(),
            name_rules: Default::default(),
        });
        app.webui.attach(&app);
        Ok(app)
    }

    /// Registers the listener that receives [`crate::CoreEvent`]s (one per app).
    pub fn set_event_listener(&self, listener: Arc<dyn EventListener>) {
        self.ctx.events.set_listener(listener);
    }

    /// Saves resume data and stops every subsystem. Blocks until done
    /// (bounded by a timeout); call once before the process exits.
    pub fn shutdown(&self) {
        let session = self.session.clone();
        let rss = self.rss.clone();
        let search = self.search.clone();
        let webui = self.webui.clone();
        let fut = async move {
            webui.stop().await;
            search.shutdown().await;
            rss.shutdown().await;
            session.shutdown().await;
        };
        self.tracker.stop();
        let _ = self.rt.block_on(async { tokio::time::timeout(std::time::Duration::from_secs(10), fut).await });
        self.ctx.logger.flush();
    }
}

impl App {
    /// Runs `fut` on the app runtime and awaits it from any executor.
    /// The task is aborted if the returned future is dropped (e.g. the Swift
    /// `Task` awaiting it is cancelled).
    pub(crate) async fn spawn_on_rt<T, F>(&self, fut: F) -> QbtResult<T>
    where
        F: std::future::Future<Output = QbtResult<T>> + Send + 'static,
        T: Send + 'static,
    {
        struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
        impl<T> Drop for AbortOnDrop<T> {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let mut guard = AbortOnDrop(self.rt.spawn(fut));
        match (&mut guard.0).await {
            Ok(r) => r,
            Err(e) if e.is_cancelled() => Err(QbtError::other("operation cancelled")),
            Err(e) => Err(QbtError::other(format!("task failed: {e}"))),
        }
    }

    pub fn context(&self) -> &CoreContext {
        &self.ctx
    }

    pub fn session(&self) -> &Arc<bittorrent::Session> {
        &self.session
    }

    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.rt
    }

    /// The WebUI server (the daemon forces it on).
    pub fn webui_controller(&self) -> &Arc<webui::WebUiController> {
        &self.webui
    }

    /// Blocks the calling (non-runtime) thread until a shutdown is requested
    /// (WebAPI `app/shutdown`, SIGINT/SIGTERM in the daemon).
    pub fn wait_for_shutdown_request(&self) {
        let rx = self.ctx.events.shutdown_requested();
        self.rt.block_on(async move {
            let mut rx = rx;
            let _ = rx.wait_for(|v| *v).await;
        });
    }
}
