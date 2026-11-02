//! Lua plugins engine (qBittorrent 5.3 `base/plugins`, PR #24485).
//!
//! Plugins are single `.lua` files in `<profile>/data/plugins`; their
//! enabled flags live in `<profile>/config/plugins.json` (qBittorrent's
//! `plugins.json` format). Each plugin gets its own sandboxed Lua 5.4
//! state. Everything runs on one dedicated worker thread fed by a bounded
//! queue: the session only checks an atomic mask and, when an enabled
//! plugin handles the event, posts a small message (dropped and logged
//! when the queue is full), so plugins never run on the core's hot paths.
//!
//! The engine is off by default (`advanced.plugins_enabled`). While it is
//! off no thread runs and no Lua state exists.
//!
//! See docs/SECURITY.md ("Lua plugins") for the threat model.

mod api;
mod datetime;
pub mod fs;
mod host;
mod manager;
mod runtime;
mod version;
mod worker;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};

use parking_lot::Mutex;
use serde::Serialize;

pub use host::{PluginHost, SessionHost, ShareLimitsValue, TorrentAction};
pub use manager::PluginsManager;
pub use version::PluginVersion;

use crate::logger::Logger;
use crate::model::{TorrentInfo, TrackerEntry, TrackerTierEntry};

/// Events queued for the worker before new ones are dropped.
pub const EVENT_QUEUE_CAPACITY: usize = 256;

/// An installed plugin as shown in the Plugins window.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PluginInfo {
    /// File name without `.lua` (qBittorrent's plugin ID).
    pub id: String,
    /// `NAME`.
    pub name: String,
    /// `VERSION` ("major.minor").
    pub version: String,
    /// Optional `AUTHOR` (qBit NeoRS extension; "" when absent).
    pub author: String,
    /// Optional `DESCRIPTION` (qBit NeoRS extension; "" when absent).
    pub description: String,
    /// Defines a global `invoke` function (shown in the Plugins menu).
    pub invocable: bool,
    pub enabled: bool,
    /// Event handlers the plugin defines (`onTorrentAdded`, ...).
    pub handlers: Vec<String>,
    pub file_path: String,
    pub file_size: i64,
    /// SHA-256 of the file, lower-case hex.
    pub sha256: String,
    /// Since the engine started: programs started with `qBittorrent.exec`.
    pub programs_run: i64,
    /// Since the engine started: e-mails sent with `qBittorrent.sendMail`.
    pub mails_sent: i64,
    /// Since the engine started: failed or stopped callbacks.
    pub failures: i64,
    /// The last failure ("" when none).
    pub last_error: String,
}

/// State of the plugins engine for the Plugins window.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PluginsStatus {
    /// The engine runs (the global switch is on and it started).
    pub running: bool,
    /// The plugins folder.
    pub folder: String,
    /// Installed plugins, sorted by ID.
    pub plugins: Vec<PluginInfo>,
    /// Folder or file problems found while loading (files that did not load).
    pub problems: Vec<String>,
}

/// Result of installing a plugin file.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PluginInstallResult {
    pub plugin: PluginInfo,
    /// An older version was replaced.
    pub updated: bool,
    /// The replaced version ("" for a new plugin).
    pub previous_version: String,
}

/// qBittorrent's event handler names, in `PluginsEngine::connectEventHandlers` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PluginHandler {
    AddTorrentFailed,
    DuplicateTorrentDetected,
    TorrentsUpdated,
    TorrentAdded,
    TorrentMetadataReady,
    TorrentFinished,
    TorrentStarted,
    TorrentStopped,
    TorrentSavePathChanged,
    TorrentSavingModeChanged,
    TorrentCategoryChanged,
    TorrentTagAdded,
    TorrentTagRemoved,
    TorrentContentFileRenamed,
    TorrentContentFolderRenamed,
    TorrentContentFolderRenamingFailed,
    TorrentIOError,
    TorrentAboutToBeRemoved,
    TorrentAnnounceSuccess,
    TorrentAnnounceWarning,
    TorrentAnnounceError,
    TorrentTrackersAdded,
    TorrentTrackersRemoved,
    TorrentTrackerStatusesUpdated,
}

impl PluginHandler {
    pub const ALL: [PluginHandler; 24] = [
        Self::AddTorrentFailed,
        Self::DuplicateTorrentDetected,
        Self::TorrentsUpdated,
        Self::TorrentAdded,
        Self::TorrentMetadataReady,
        Self::TorrentFinished,
        Self::TorrentStarted,
        Self::TorrentStopped,
        Self::TorrentSavePathChanged,
        Self::TorrentSavingModeChanged,
        Self::TorrentCategoryChanged,
        Self::TorrentTagAdded,
        Self::TorrentTagRemoved,
        Self::TorrentContentFileRenamed,
        Self::TorrentContentFolderRenamed,
        Self::TorrentContentFolderRenamingFailed,
        Self::TorrentIOError,
        Self::TorrentAboutToBeRemoved,
        Self::TorrentAnnounceSuccess,
        Self::TorrentAnnounceWarning,
        Self::TorrentAnnounceError,
        Self::TorrentTrackersAdded,
        Self::TorrentTrackersRemoved,
        Self::TorrentTrackerStatusesUpdated,
    ];

    /// The global Lua function called for the event.
    pub fn name(self) -> &'static str {
        match self {
            Self::AddTorrentFailed => "onAddTorrentFailed",
            Self::DuplicateTorrentDetected => "onDuplicateTorrentDetected",
            Self::TorrentsUpdated => "onTorrentsUpdated",
            Self::TorrentAdded => "onTorrentAdded",
            Self::TorrentMetadataReady => "onTorrentMetadataReady",
            Self::TorrentFinished => "onTorrentFinished",
            Self::TorrentStarted => "onTorrentStarted",
            Self::TorrentStopped => "onTorrentStopped",
            Self::TorrentSavePathChanged => "onTorrentSavePathChanged",
            Self::TorrentSavingModeChanged => "onTorrentSavingModeChanged",
            Self::TorrentCategoryChanged => "onTorrentCategoryChanged",
            Self::TorrentTagAdded => "onTorrentTagAdded",
            Self::TorrentTagRemoved => "onTorrentTagRemoved",
            Self::TorrentContentFileRenamed => "onTorrentContentFileRenamed",
            Self::TorrentContentFolderRenamed => "onTorrentContentFolderRenamed",
            Self::TorrentContentFolderRenamingFailed => "onTorrentContentFolderRenamingFailed",
            Self::TorrentIOError => "onTorrentIOError",
            Self::TorrentAboutToBeRemoved => "onTorrentAboutToBeRemoved",
            Self::TorrentAnnounceSuccess => "onTorrentAnnounceSuccess",
            Self::TorrentAnnounceWarning => "onTorrentAnnounceWarning",
            Self::TorrentAnnounceError => "onTorrentAnnounceError",
            Self::TorrentTrackersAdded => "onTorrentTrackersAdded",
            Self::TorrentTrackersRemoved => "onTorrentTrackersRemoved",
            Self::TorrentTrackerStatusesUpdated => "onTorrentTrackerStatusesUpdated",
        }
    }

    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// A torrent passed to a handler: looked up live by ID when the plugin
/// reads it, falling back to the copy taken when the event happened (a
/// torrent about to be removed is gone by the time the worker runs).
#[derive(Clone, Debug)]
pub struct TorrentArg {
    pub id: String,
    pub captured: Option<Arc<TorrentInfo>>,
}

/// Handler arguments after the torrent (qBittorrent's signal arguments).
#[derive(Clone, Debug)]
pub enum PluginArg {
    Torrent(TorrentArg),
    Str(String),
    Int(i64),
    /// A list of strings (removed tracker URLs).
    StrList(Vec<String>),
    /// File index -> path (`QHash<int, Path>`).
    IndexedPaths(Vec<(i64, String)>),
    /// A list of integers (failed file indexes).
    IntList(Vec<i64>),
    /// Added trackers (`TrackerEntry` objects with `url` and `tier`).
    Trackers(Vec<TrackerTierEntry>),
    /// Tracker URL -> status (`QHash<QString, TrackerEntryStatus>`).
    TrackerStatuses(Vec<TrackerEntry>),
    /// A list of torrents (`onTorrentsUpdated`).
    Torrents(Vec<TorrentArg>),
}

pub(crate) enum Job {
    Event { handler: PluginHandler, args: Vec<PluginArg> },
    Control(manager::Control),
}

/// The session's side of the engine: checks whether any enabled plugin
/// handles an event and queues it without blocking. Cheap to clone; a
/// no-op (one relaxed atomic load) while plugins are off.
#[derive(Clone, Default)]
pub struct PluginBus {
    inner: Arc<BusInner>,
}

#[derive(Default)]
struct BusInner {
    /// Bits of the handlers defined by enabled plugins.
    mask: AtomicU32,
    tx: Mutex<Option<SyncSender<Job>>>,
    dropped: AtomicU64,
}

impl PluginBus {
    /// Whether an enabled plugin defines the handler.
    #[inline]
    pub fn wants(&self, handler: PluginHandler) -> bool {
        self.inner.mask.load(Ordering::Relaxed) & handler.bit() != 0
    }

    pub(crate) fn set_mask(&self, mask: u32) {
        self.inner.mask.store(mask, Ordering::Relaxed);
    }

    pub(crate) fn connect(&self, tx: Option<SyncSender<Job>>) {
        if tx.is_none() {
            self.set_mask(0);
        }
        *self.inner.tx.lock() = tx;
    }

    /// Queues an event; when the queue is full the event is dropped and a
    /// warning logged (the first drop, then every 100th).
    pub fn emit(&self, handler: PluginHandler, args: Vec<PluginArg>, logger: &Logger) {
        if !self.wants(handler) {
            return;
        }
        let tx = self.inner.tx.lock().clone();
        let Some(tx) = tx else { return };
        match tx.try_send(Job::Event { handler, args }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                if n.is_multiple_of(100) {
                    logger.warning(format!(
                        "Plugins are too slow; dropped event \"{}\" ({} dropped so far).",
                        handler.name(),
                        n + 1
                    ));
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Events dropped because the queue was full.
    pub fn dropped_events(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests;
