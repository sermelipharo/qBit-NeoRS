use std::path::PathBuf;

/// Start-up configuration supplied by the front end.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AppConfig {
    /// Root folder for configuration and data. Empty = the platform default
    /// (`~/Library/Application Support/qBit NeoRS` on macOS).
    pub profile_dir: String,
    /// Also print log lines to stderr (headless daemon, debugging).
    pub log_to_stderr: bool,
    /// Override of the WebUI port from the command line (`--webui-port`); 0 = keep preference.
    pub webui_port_override: u16,
    /// Front end name for logs/User-Agent ("macOS", "nox").
    pub front_end: String,
}

/// Resolved profile folders. Mirrors qBittorrent's layout so concepts map
/// one to one (`BT_backup`, `rss`, `nova3`, `logs`, `GeoDB`).
#[derive(Clone, Debug)]
pub struct Profile {
    pub root: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

impl Profile {
    pub fn resolve(config: &AppConfig) -> Self {
        let root = if config.profile_dir.is_empty() {
            default_profile_root()
        } else {
            PathBuf::from(&config.profile_dir)
        };
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            root,
        }
    }

    /// `config/preferences.json`
    pub fn preferences_file(&self) -> PathBuf {
        self.config_dir.join("preferences.json")
    }
    /// `config/categories.json`
    pub fn categories_file(&self) -> PathBuf {
        self.config_dir.join("categories.json")
    }
    /// `data/BT_backup` — per-torrent `.torrent` + `.fastresume` (JSON) files.
    pub fn bt_backup_dir(&self) -> PathBuf {
        self.data_dir.join("BT_backup")
    }
    /// `data/rss` — feeds.json, articles/, download_rules.json.
    pub fn rss_dir(&self) -> PathBuf {
        self.data_dir.join("rss")
    }
    /// `data/nova3` — search engine helpers and plugins.
    pub fn search_dir(&self) -> PathBuf {
        self.data_dir.join("nova3")
    }
    /// `data/logs` — default file log folder.
    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }
    /// `data/GeoDB` — DB-IP country database.
    pub fn geoip_dir(&self) -> PathBuf {
        self.data_dir.join("GeoDB")
    }
    /// `data/stats.json` — all-time statistics.
    pub fn stats_file(&self) -> PathBuf {
        self.data_dir.join("stats.json")
    }
    /// `data/engine` — engine state (DHT routing table, ...).
    pub fn engine_dir(&self) -> PathBuf {
        self.data_dir.join("engine")
    }
}

fn default_profile_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        let support = home.join("Library/Application Support");
        let root = support.join("qBit NeoRS");
        // The product was called "qBittorrent Neo" before the rename; move an
        // existing default profile over once (never for an explicit profile dir).
        let legacy = support.join("qBittorrent Neo");
        if legacy.is_dir() && !root.exists() {
            let _ = std::fs::rename(&legacy, &root);
        }
        root
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("qbit-neors")
    }
}
