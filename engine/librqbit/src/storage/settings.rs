//! Session-wide storage settings that can change at runtime.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};

use super::{paths::validate_suffix, pool::FilePool};

/// OS page cache usage for disk I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskCacheMode {
    /// Normal buffered I/O.
    #[default]
    Enabled,
    /// Bypass the OS cache where the platform supports it (`F_NOCACHE` on macOS).
    Disabled,
    /// Writes only: flush the files a piece touched once the piece is complete.
    WriteThrough,
}

/// Storage settings shared by all torrents of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    /// When set, files that are not complete yet carry this suffix on disk
    /// (e.g. `".!qB"`). They are renamed to their final name as soon as they
    /// complete, and all files are renamed when the option changes.
    pub incomplete_suffix: Option<String>,
    /// Keep files that are not wanted (priority 0) in a `.unwanted` subfolder of
    /// their directory; they move back when wanted again.
    pub use_unwanted_folder: bool,
    /// Allocate full file sizes when a torrent starts instead of sparse files.
    pub preallocate: bool,
    /// Maximum number of file handles kept open (session-wide LRU).
    pub file_pool_size: usize,
    /// OS cache mode for reads (`Enabled` or `Disabled`).
    pub read_cache: DiskCacheMode,
    /// OS cache mode for writes (`Enabled`, `Disabled` or `WriteThrough`).
    pub write_cache: DiskCacheMode,
}

pub const DEFAULT_FILE_POOL_SIZE: usize = 100;

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            incomplete_suffix: None,
            use_unwanted_folder: false,
            preallocate: false,
            file_pool_size: DEFAULT_FILE_POOL_SIZE,
            read_cache: DiskCacheMode::Enabled,
            write_cache: DiskCacheMode::Enabled,
        }
    }
}

impl StorageSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(s) = &self.incomplete_suffix {
            validate_suffix(s).map_err(|e| anyhow::anyhow!("invalid incomplete suffix: {e}"))?;
        }
        if self.file_pool_size == 0 {
            anyhow::bail!("file_pool_size must be at least 1");
        }
        Ok(())
    }

    /// The suffix to use, if the option is on.
    pub fn suffix(&self) -> Option<&str> {
        self.incomplete_suffix.as_deref().filter(|s| !s.is_empty())
    }

    /// The suffix to look for when finding existing data: the configured one, or
    /// the conventional one when the option is off (files may be left over from a
    /// run with the option on).
    pub fn search_suffix(&self) -> &str {
        self.suffix().unwrap_or(DEFAULT_INCOMPLETE_SUFFIX)
    }
}

/// qBittorrent's incomplete-file suffix.
pub const DEFAULT_INCOMPLETE_SUFFIX: &str = ".!qB";

/// Session-wide storage state: settings and the shared file handle pool.
pub(crate) struct StorageContext {
    settings: ArcSwap<StorageSettings>,
    pub(crate) pool: FilePool,
    next_uid: AtomicU64,
}

/// What changed when settings were replaced.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SettingsChange {
    /// Suffix or unwanted folder option changed: on-disk names need reconciling.
    pub layout: bool,
}

impl StorageContext {
    pub fn new(settings: StorageSettings) -> Arc<Self> {
        Arc::new(Self {
            pool: FilePool::new(settings.file_pool_size),
            settings: ArcSwap::from_pointee(settings),
            next_uid: AtomicU64::new(1),
        })
    }

    pub fn settings(&self) -> Arc<StorageSettings> {
        self.settings.load_full()
    }

    pub fn set_settings(&self, new: StorageSettings) -> SettingsChange {
        let old = self.settings.swap(Arc::new(new.clone()));
        if old.file_pool_size != new.file_pool_size {
            self.pool.set_capacity(new.file_pool_size);
        }
        if old.read_cache != new.read_cache || old.write_cache != new.write_cache {
            // Handles carry the cache flags they were opened with.
            self.pool.clear();
        }
        SettingsChange {
            layout: old.suffix() != new.suffix()
                || old.use_unwanted_folder != new.use_unwanted_folder,
        }
    }

    pub fn next_uid(&self) -> u64 {
        self.next_uid.fetch_add(1, Ordering::Relaxed)
    }
}
