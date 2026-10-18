use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::{broadcast, watch};

use crate::model::LogMessage;

/// Notifications pushed from the core to front ends (GUI notifications, log
/// view updates, dialogs). Polling-based data (transfer list, properties)
/// does not go through events.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CoreEvent {
    TorrentAdded { hash: String, name: String },
    /// Fired when a torrent finishes downloading (not on resume of a complete torrent).
    TorrentFinished { hash: String, name: String },
    TorrentMetadataReceived { hash: String, name: String },
    TorrentRemoved { hash: String, name: String },
    /// Torrent stopped because of an I/O or engine error.
    TorrentError { hash: String, name: String, message: String },
    /// Adding failed (bad file, download error, ...).
    AddTorrentFailed { source: String, reason: String },
    /// The torrent already exists; `merged_trackers` tells whether new trackers
    /// were merged into it (preference "merge trackers").
    DuplicateTorrent { hash: String, name: String, merged_trackers: bool },
    /// Every torrent finished downloading (auto-shutdown hook).
    AllTorrentsFinished,
    /// Free disk space is too low / write failure.
    FullDiskError { hash: String, name: String, message: String },
    /// RSS auto-downloader added a torrent.
    RssTorrentDownloaded { rule: String, article_title: String },
    /// RSS feeds changed (new articles, loading state).
    RssUpdated,
    /// A search job produced new results or finished.
    SearchUpdated { id: i64 },
    /// Search plugins finished installing/updating.
    SearchPluginsUpdated { message: String },
    /// A torrent-creator task changed state.
    CreatorTaskUpdated { id: String },
    /// New log line (the log window appends without polling).
    Log { message: LogMessage },
    /// Preferences changed (by the WebAPI or another front end).
    PreferencesChanged,
    /// External IP address detected/changed.
    ExternalAddressChanged { address: String },
    /// The BitTorrent session was paused (no torrent transfers).
    SessionPaused,
    /// The BitTorrent session was resumed.
    SessionResumed,
    /// A WebAPI client or signal asked the application to quit.
    ShutdownRequested,
}

/// Receives core events. Implemented in Swift by the app; called from core
/// threads, so implementations must hop to the main actor themselves.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait EventListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
}

/// Fan-out of [`CoreEvent`]s to the registered foreign listener and to
/// in-process subscribers (WebUI, daemon).
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

struct EventBusInner {
    listener: RwLock<Option<Arc<dyn EventListener>>>,
    tx: broadcast::Sender<CoreEvent>,
    shutdown: watch::Sender<bool>,
}

impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(256);
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(EventBusInner { listener: RwLock::new(None), tx, shutdown }),
        }
    }
}

impl EventBus {
    pub fn set_listener(&self, listener: Arc<dyn EventListener>) {
        *self.inner.listener.write() = Some(listener);
    }

    pub fn emit(&self, event: CoreEvent) {
        let _ = self.inner.tx.send(event.clone());
        let listener = self.inner.listener.read().clone();
        if let Some(l) = listener {
            l.on_event(event);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.inner.tx.subscribe()
    }

    pub fn request_shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        self.emit(CoreEvent::ShutdownRequested);
    }

    pub fn shutdown_requested(&self) -> watch::Receiver<bool> {
        self.inner.shutdown.subscribe()
    }
}
