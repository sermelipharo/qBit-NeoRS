use serde::{Serialize, Serializer};

use super::{Category, TorrentInfo};

/// Listening/connectivity status shown in the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "lowercase")]
pub enum ConnectionStatus {
    Connected,
    Firewalled,
    #[default]
    Disconnected,
}

fn ratio_string<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:.2}"))
}

fn percent_string<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&format!("{v:.2}"))
}

/// Status bar + statistics dialog data; `server_state` of `/api/v2/sync/maindata`
/// and `/api/v2/transfer/info`.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct ServerState {
    #[serde(rename = "dl_info_speed")]
    pub dl_speed: i64,
    #[serde(rename = "dl_info_data")]
    pub dl_session_data: i64,
    #[serde(rename = "up_info_speed")]
    pub up_speed: i64,
    #[serde(rename = "up_info_data")]
    pub up_session_data: i64,
    /// Effective global limits (alt limits when enabled), 0 = unlimited.
    pub dl_rate_limit: i64,
    pub up_rate_limit: i64,
    pub dht_nodes: i64,
    pub connection_status: ConnectionStatus,
    pub queueing: bool,
    pub use_alt_speed_limits: bool,
    pub use_subcategories: bool,
    pub refresh_interval: i64,
    pub free_space_on_disk: i64,
    pub last_external_address_v4: String,
    pub last_external_address_v6: String,
    #[serde(rename = "alltime_dl")]
    pub alltime_download: i64,
    #[serde(rename = "alltime_ul")]
    pub alltime_upload: i64,
    pub total_wasted_session: i64,
    #[serde(serialize_with = "ratio_string")]
    pub global_ratio: f64,
    pub total_peer_connections: i64,
    // Disk statistics (statistics dialog "Cache statistics" / "Performance statistics").
    #[serde(serialize_with = "percent_string")]
    pub read_cache_hits: f64,
    pub total_buffers_size: i64,
    #[serde(serialize_with = "percent_string")]
    pub write_cache_overload: f64,
    #[serde(serialize_with = "percent_string")]
    pub read_cache_overload: f64,
    pub queued_io_jobs: i64,
    pub average_time_queue: i64,
    pub total_queued_size: i64,
}

/// One sample of the "Speed" graph (bytes/s), taken every second.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SpeedSample {
    /// Unix milliseconds.
    pub timestamp_ms: i64,
    pub upload: i64,
    pub download: i64,
    pub payload_upload: i64,
    pub payload_download: i64,
    pub overhead_upload: i64,
    pub overhead_download: i64,
    pub dht_upload: i64,
    pub dht_download: i64,
    pub tracker_upload: i64,
    pub tracker_download: i64,
}

/// Incremental update for the GUI transfer list. Unlike the WebAPI (which
/// diffs per field), changed torrents are sent as complete records.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MainDataUpdate {
    pub rid: i64,
    pub full_update: bool,
    pub torrents: Vec<TorrentInfo>,
    pub torrents_removed: Vec<String>,
    pub categories: Vec<Category>,
    pub categories_removed: Vec<String>,
    pub tags: Vec<String>,
    pub tags_removed: Vec<String>,
    /// Tracker host/URL -> torrent hashes using it (sidebar tracker filter).
    pub trackers: Vec<TrackerTorrents>,
    pub trackers_removed: Vec<String>,
    pub server_state: ServerState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TrackerTorrents {
    pub url: String,
    pub hashes: Vec<String>,
}

/// Network interface for the "Network interface" advanced setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct NetworkInterface {
    pub name: String,
    pub value: String,
    pub addresses: Vec<String>,
}

/// A cookie from the cookie manager (`/api/v2/app/cookies`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Cookie {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub value: String,
    /// Unix seconds, 0 = session cookie.
    #[serde(rename = "expirationDate")]
    pub expiration_date: i64,
}

/// Build information for the About dialog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct BuildInfo {
    /// This application's version.
    pub app: String,
    /// qBittorrent version the feature set is compatible with.
    pub qbittorrent_compat: String,
    pub webapi: String,
    pub engine: String,
    pub rustc: String,
    pub platform: String,
    pub bitness: i32,
}
