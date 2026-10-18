use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "lowercase")]
pub enum TorrentFormat {
    V1,
    V2,
    #[default]
    Hybrid,
}

/// Torrent Creator dialog / `/api/v2/torrentcreator/addTask`.
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentCreatorParams {
    pub source_path: String,
    /// Where to write the .torrent; empty = keep in memory only.
    pub torrent_file_path: String,
    pub format: TorrentFormat,
    /// Bytes; 0 = automatic.
    pub piece_size: i64,
    pub optimize_alignment: bool,
    /// -1 = disabled.
    pub padding_file_limit: i64,
    pub private: bool,
    pub start_seeding: bool,
    pub ignore_share_limits: bool,
    /// Tracker URLs; an empty string starts a new tier.
    pub trackers: Vec<String>,
    pub url_seeds: Vec<String>,
    pub comment: String,
    pub source: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CreatorTaskStatus {
    #[default]
    Queued,
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct CreatorTask {
    #[serde(rename = "taskID")]
    pub id: String,
    pub source_path: String,
    pub status: CreatorTaskStatus,
    /// 0.0 ..= 100.0
    pub progress: f64,
    pub error_message: String,
    pub torrent_file_path: String,
    pub piece_size: i64,
    pub format: TorrentFormat,
    pub time_added: i64,
    pub time_started: i64,
    pub time_finished: i64,
    /// Info hash of the created torrent when finished.
    pub info_hash: String,
}
