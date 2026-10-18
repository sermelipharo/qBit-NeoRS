use serde::{Deserialize, Serialize, Serializer};

/// Torrent state exactly as qBittorrent's `BitTorrent::TorrentState`; serde
/// names are the WebAPI strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TorrentState {
    #[default]
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "forcedDL")]
    ForcedDownloading,
    #[serde(rename = "downloading")]
    Downloading,
    #[serde(rename = "forcedMetaDL")]
    ForcedDownloadingMetadata,
    #[serde(rename = "metaDL")]
    DownloadingMetadata,
    #[serde(rename = "stalledDL")]
    StalledDownloading,
    #[serde(rename = "forcedUP")]
    ForcedUploading,
    #[serde(rename = "uploading")]
    Uploading,
    #[serde(rename = "stalledUP")]
    StalledUploading,
    #[serde(rename = "checkingResumeData")]
    CheckingResumeData,
    #[serde(rename = "queuedDL")]
    QueuedDownloading,
    #[serde(rename = "queuedUP")]
    QueuedUploading,
    #[serde(rename = "checkingUP")]
    CheckingUploading,
    #[serde(rename = "checkingDL")]
    CheckingDownloading,
    #[serde(rename = "stoppedDL")]
    StoppedDownloading,
    #[serde(rename = "stoppedUP")]
    StoppedUploading,
    #[serde(rename = "moving")]
    Moving,
    #[serde(rename = "missingFiles")]
    MissingFiles,
    #[serde(rename = "error")]
    Error,
}

impl TorrentState {
    pub fn is_downloading(self) -> bool {
        use TorrentState::*;
        matches!(
            self,
            Downloading
                | DownloadingMetadata
                | ForcedDownloadingMetadata
                | StalledDownloading
                | CheckingDownloading
                | StoppedDownloading
                | QueuedDownloading
                | ForcedDownloading
        )
    }

    pub fn is_uploading(self) -> bool {
        use TorrentState::*;
        matches!(self, Uploading | StalledUploading | CheckingUploading | QueuedUploading | ForcedUploading)
    }

    pub fn is_completed(self) -> bool {
        use TorrentState::*;
        matches!(
            self,
            Uploading | StalledUploading | CheckingUploading | StoppedUploading | QueuedUploading | ForcedUploading
        )
    }

    pub fn is_stopped(self) -> bool {
        matches!(self, TorrentState::StoppedDownloading | TorrentState::StoppedUploading)
    }

    pub fn is_running(self) -> bool {
        !self.is_stopped()
    }

    pub fn is_queued(self) -> bool {
        matches!(self, TorrentState::QueuedDownloading | TorrentState::QueuedUploading)
    }

    pub fn is_checking(self) -> bool {
        matches!(
            self,
            TorrentState::CheckingDownloading | TorrentState::CheckingUploading | TorrentState::CheckingResumeData
        )
    }

    pub fn is_errored(self) -> bool {
        matches!(self, TorrentState::Error | TorrentState::MissingFiles)
    }

    pub fn is_stalled(self) -> bool {
        matches!(self, TorrentState::StalledDownloading | TorrentState::StalledUploading)
    }

    pub fn is_moving(self) -> bool {
        self == TorrentState::Moving
    }

    pub fn as_api_str(self) -> &'static str {
        use TorrentState::*;
        match self {
            Unknown => "unknown",
            ForcedDownloading => "forcedDL",
            Downloading => "downloading",
            ForcedDownloadingMetadata => "forcedMetaDL",
            DownloadingMetadata => "metaDL",
            StalledDownloading => "stalledDL",
            ForcedUploading => "forcedUP",
            Uploading => "uploading",
            StalledUploading => "stalledUP",
            CheckingResumeData => "checkingResumeData",
            QueuedDownloading => "queuedDL",
            QueuedUploading => "queuedUP",
            CheckingUploading => "checkingUP",
            CheckingDownloading => "checkingDL",
            StoppedDownloading => "stoppedDL",
            StoppedUploading => "stoppedUP",
            Moving => "moving",
            MissingFiles => "missingFiles",
            Error => "error",
        }
    }
}

/// Status filter buckets of the sidebar and of `/api/v2/torrents/info?filter=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
#[serde(rename_all = "snake_case")]
pub enum StatusFilter {
    #[default]
    All,
    Downloading,
    Seeding,
    Completed,
    Running,
    Stopped,
    Active,
    Inactive,
    Stalled,
    StalledUploading,
    StalledDownloading,
    Checking,
    Moving,
    Errored,
}

/// `DownloadPriority`: values are the libtorrent/qBittorrent integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum FilePriority {
    /// "Do not download" (0)
    Ignored,
    /// 1
    #[default]
    Normal,
    /// 6
    High,
    /// 7
    Maximum,
}

impl FilePriority {
    pub fn as_i32(self) -> i32 {
        match self {
            FilePriority::Ignored => 0,
            FilePriority::Normal => 1,
            FilePriority::High => 6,
            FilePriority::Maximum => 7,
        }
    }

    /// Maps any libtorrent priority (0..=7) to the four qBittorrent levels.
    pub fn from_i32(v: i32) -> Option<Self> {
        Some(match v {
            0 => FilePriority::Ignored,
            1..=5 => FilePriority::Normal,
            6 => FilePriority::High,
            7 => FilePriority::Maximum,
            _ => return None,
        })
    }
}

impl Serialize for FilePriority {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(self.as_i32())
    }
}

impl<'de> Deserialize<'de> for FilePriority {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = i32::deserialize(d)?;
        FilePriority::from_i32(v).ok_or_else(|| serde::de::Error::custom(format!("invalid priority {v}")))
    }
}

/// `TorrentContentLayout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ContentLayout {
    #[default]
    Original,
    Subfolder,
    NoSubfolder,
}

/// `Torrent::StopCondition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum StopCondition {
    #[default]
    None,
    MetadataReceived,
    FilesChecked,
}

/// `ShareLimitAction`; `Default` defers to the global setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum ShareLimitAction {
    #[default]
    Default,
    Stop,
    Remove,
    RemoveWithContent,
    EnableSuperSeeding,
}

impl ShareLimitAction {
    /// Integer used by the `max_ratio_act` preference.
    pub fn as_i32(self) -> i32 {
        match self {
            ShareLimitAction::Default => -1,
            ShareLimitAction::Stop => 0,
            ShareLimitAction::Remove => 1,
            ShareLimitAction::EnableSuperSeeding => 2,
            ShareLimitAction::RemoveWithContent => 3,
        }
    }

    pub fn from_i32(v: i32) -> Self {
        match v {
            0 => ShareLimitAction::Stop,
            1 => ShareLimitAction::Remove,
            2 => ShareLimitAction::EnableSuperSeeding,
            3 => ShareLimitAction::RemoveWithContent,
            _ => ShareLimitAction::Default,
        }
    }
}

fn join_tags<S: Serializer>(tags: &[String], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&tags.join(", "))
}

/// One row of the transfer list; `/api/v2/torrents/info` item.
///
/// Sizes are bytes, speeds bytes/s, times seconds, dates Unix seconds
/// (`-1`/`0` when unset, as qBittorrent does).
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentInfo {
    /// Torrent ID: the v2 info hash truncated to 20 bytes (hex) for v2 and
    /// hybrid torrents, the v1 info hash otherwise.
    pub hash: String,
    pub infohash_v1: String,
    pub infohash_v2: String,
    pub name: String,
    pub has_metadata: bool,
    pub created_by: String,
    pub creation_date: i64,
    /// `None` until metadata is known.
    pub private: Option<bool>,
    /// Size of the whole torrent.
    pub total_size: i64,
    pub pieces_num: i64,
    pub piece_size: i64,
    pub magnet_uri: String,
    /// Size of the selected (wanted) files.
    pub size: i64,
    /// 0.0 ..= 1.0
    pub progress: f64,
    pub total_wasted: i64,
    pub pieces_have: i64,
    #[serde(rename = "dlspeed")]
    pub dl_speed: i64,
    #[serde(rename = "upspeed")]
    pub up_speed: i64,
    /// Queue position (1-based); 0 when not queued / queueing disabled.
    #[serde(rename = "priority")]
    pub queue_position: i64,
    /// Connected seeds.
    pub num_seeds: i64,
    /// Seeds in the swarm (from trackers), -1 unknown.
    pub num_complete: i64,
    /// Connected leechers.
    pub num_leechs: i64,
    /// Leechers in the swarm, -1 unknown.
    pub num_incomplete: i64,
    pub state: TorrentState,
    pub eta: i64,
    pub seq_dl: bool,
    pub f_l_piece_prio: bool,
    pub category: String,
    #[serde(serialize_with = "join_tags")]
    pub tags: Vec<String>,
    pub super_seeding: bool,
    pub force_start: bool,
    pub save_path: String,
    pub download_path: String,
    pub content_path: String,
    pub root_path: String,
    pub added_on: i64,
    pub completion_on: i64,
    /// Current working tracker URL.
    pub tracker: String,
    pub trackers_count: i64,
    /// bytes/s, -1 or 0 = unlimited.
    pub dl_limit: i64,
    pub up_limit: i64,
    pub downloaded: i64,
    pub uploaded: i64,
    pub downloaded_session: i64,
    pub uploaded_session: i64,
    pub amount_left: i64,
    pub completed: i64,
    pub connections_count: i64,
    pub connections_limit: i64,
    /// Effective ratio limit (after applying global defaults); -1 none.
    pub max_ratio: f64,
    /// Effective seeding time limit in minutes; -1 none.
    pub max_seeding_time: i64,
    pub max_inactive_seeding_time: i64,
    /// -1 when >= MAX_RATIO.
    pub ratio: f64,
    /// Per-torrent ratio limit: -2 use global, -1 unlimited.
    pub ratio_limit: f64,
    pub popularity: f64,
    /// Minutes: -2 use global, -1 unlimited.
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    pub share_limit_action: ShareLimitAction,
    #[serde(rename = "seen_complete")]
    pub last_seen_complete: i64,
    pub last_activity: i64,
    pub auto_tmm: bool,
    pub time_active: i64,
    pub seeding_time: i64,
    /// Distributed copies, -1 when unknown.
    pub availability: f64,
    /// Seconds until next announce.
    pub reannounce: i64,
    pub comment: String,
    pub has_tracker_warning: bool,
    pub has_tracker_error: bool,
    pub has_other_announce_error: bool,
    /// Last error message (GUI status tooltip); not part of the WebAPI.
    #[serde(skip)]
    pub error_message: String,
}

/// General tab / `/api/v2/torrents/properties`.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentProperties {
    pub hash: String,
    pub infohash_v1: String,
    pub infohash_v2: String,
    pub name: String,
    pub time_elapsed: i64,
    pub seeding_time: i64,
    pub eta: i64,
    #[serde(rename = "nb_connections")]
    pub connections: i64,
    #[serde(rename = "nb_connections_limit")]
    pub connections_limit: i64,
    pub total_downloaded: i64,
    pub total_downloaded_session: i64,
    pub total_uploaded: i64,
    pub total_uploaded_session: i64,
    pub dl_speed: i64,
    pub dl_speed_avg: i64,
    pub up_speed: i64,
    pub up_speed_avg: i64,
    pub dl_limit: i64,
    pub up_limit: i64,
    pub total_wasted: i64,
    pub seeds: i64,
    pub seeds_total: i64,
    pub peers: i64,
    pub peers_total: i64,
    pub share_ratio: f64,
    pub popularity: f64,
    pub reannounce: i64,
    pub total_size: i64,
    pub pieces_num: i64,
    pub piece_size: i64,
    pub pieces_have: i64,
    pub created_by: String,
    pub last_seen: i64,
    pub addition_date: i64,
    pub completion_date: i64,
    pub creation_date: i64,
    pub save_path: String,
    pub download_path: String,
    pub comment: String,
    pub private: Option<bool>,
    pub has_metadata: bool,
    pub progress: f64,
    /// Distributed copies.
    pub availability: f64,
}

/// `TrackerEndpointState` plus the WebAPI-only values 0 (disabled, used for
/// DHT/PeX/LSD rows) and 3 (updating).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TrackerStatus {
    Disabled,
    #[default]
    NotContacted,
    Working,
    Updating,
    NotWorking,
    TrackerError,
    Unreachable,
}

impl TrackerStatus {
    pub fn as_i32(self) -> i32 {
        match self {
            TrackerStatus::Disabled => 0,
            TrackerStatus::NotContacted => 1,
            TrackerStatus::Working => 2,
            TrackerStatus::Updating => 3,
            TrackerStatus::NotWorking => 4,
            TrackerStatus::TrackerError => 5,
            TrackerStatus::Unreachable => 6,
        }
    }
}

impl Serialize for TrackerStatus {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(self.as_i32())
    }
}

/// Per-endpoint (local listen address x BitTorrent version) tracker status.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TrackerEndpoint {
    pub name: String,
    pub updating: bool,
    pub status: TrackerStatus,
    #[serde(rename = "msg")]
    pub message: String,
    pub bt_version: i32,
    pub num_peers: i64,
    pub num_seeds: i64,
    pub num_leeches: i64,
    pub num_downloaded: i64,
    pub next_announce: i64,
    pub min_announce: i64,
}

/// Trackers tab row / `/api/v2/torrents/trackers` item. The pseudo rows
/// `** [DHT] **`, `** [PeX] **` and `** [LSD] **` use tier -1.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TrackerEntry {
    pub url: String,
    pub tier: i64,
    pub status: TrackerStatus,
    pub updating: bool,
    #[serde(rename = "msg")]
    pub message: String,
    pub num_peers: i64,
    pub num_seeds: i64,
    pub num_leeches: i64,
    pub num_downloaded: i64,
    pub next_announce: i64,
    pub min_announce: i64,
    pub endpoints: Vec<TrackerEndpoint>,
}

/// A tracker URL with its tier, used when adding/replacing trackers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TrackerTierEntry {
    pub url: String,
    pub tier: i64,
}

/// Content tab row / `/api/v2/torrents/files` item.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentFile {
    pub index: i64,
    /// Relative path with `/` separators, as stored in the torrent (after renames).
    pub name: String,
    pub size: i64,
    pub progress: f64,
    pub priority: FilePriority,
    pub is_seed: bool,
    /// First and last piece index (inclusive).
    pub piece_range: Vec<i64>,
    /// Availability of the file's pieces in the swarm, -1 unknown.
    pub availability: f64,
}

/// Piece state for the progress bars: 0 = missing, 1 = downloading, 2 = have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum PieceState {
    #[default]
    Missing,
    Downloading,
    Downloaded,
}

/// Peers tab row / `/api/v2/sync/torrentPeers` value.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PeerInfo {
    pub ip: String,
    pub port: u16,
    pub client: String,
    pub peer_id_client: String,
    /// "BT", "μTP", "Web", or with " (I2P)".
    #[serde(rename = "connection")]
    pub connection_type: String,
    pub country: String,
    pub country_code: String,
    pub dl_speed: i64,
    pub up_speed: i64,
    pub downloaded: i64,
    pub uploaded: i64,
    pub progress: f64,
    pub relevance: f64,
    /// qBittorrent flag letters separated by spaces, e.g. "D U H E".
    pub flags: String,
    /// One "<letter> = <description>" per line.
    #[serde(rename = "flags_desc")]
    pub flags_description: String,
    /// Names of files the peer is currently exchanging, newline separated.
    pub files: String,
    pub host_name: String,
}

/// Per-torrent peer discovery switches ("Disable DHT/PeX/LSD for this
/// torrent" in the Torrent Options dialog). Private torrents never use DHT,
/// PeX or LSD, whatever the switches say.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentDiscoveryOptions {
    pub hash: String,
    pub private: bool,
    pub disable_dht: bool,
    pub disable_pex: bool,
    pub disable_lsd: bool,
}

/// HTTP Sources tab row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct WebSeed {
    pub url: String,
}

/// Category with its save paths and share limits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Category {
    pub name: String,
    /// Empty = derived from the default save path (+ category name).
    #[serde(rename = "savePath")]
    pub save_path: String,
    pub download_path: CategoryDownloadPath,
    /// -2 = inherit (parent category, then global), -1 = unlimited.
    #[serde(default = "category_default_ratio")]
    pub ratio_limit: f64,
    /// Minutes; -2 = inherit, -1 = unlimited.
    #[serde(default = "category_default_time")]
    pub seeding_time_limit: i64,
    /// Minutes; -2 = inherit, -1 = unlimited.
    #[serde(default = "category_default_time")]
    pub inactive_seeding_time_limit: i64,
    /// `Default` = inherit.
    #[serde(default)]
    pub share_limit_action: ShareLimitAction,
}

fn category_default_ratio() -> f64 {
    -2.0
}

fn category_default_time() -> i64 {
    -2
}

impl Default for Category {
    fn default() -> Self {
        Self {
            name: String::new(),
            save_path: String::new(),
            download_path: CategoryDownloadPath::Default,
            ratio_limit: -2.0,
            seeding_time_limit: -2,
            inactive_seeding_time_limit: -2,
            share_limit_action: ShareLimitAction::Default,
        }
    }
}

/// A category's incomplete-download path option.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum CategoryDownloadPath {
    /// Follow the global "Keep incomplete torrents in" setting.
    #[default]
    Default,
    /// Never use a separate download path for this category.
    Disabled,
    /// Use this path (empty = derived from the global download path).
    Path { path: String },
}

/// Where a torrent to add comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum TorrentSource {
    /// A magnet URI, a bare info hash, or an http(s) URL of a .torrent file.
    Url { url: String },
    /// Path to a local .torrent file.
    File { path: String },
    /// Raw .torrent file content.
    Bytes { data: Vec<u8> },
}

/// `AddTorrentParams`: everything the "Add new torrent" dialog and
/// `/api/v2/torrents/add` can set. `None` means "use the session default".
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct AddTorrentParams {
    pub name: Option<String>,
    pub category: Option<String>,
    pub tags: Vec<String>,
    pub save_path: Option<String>,
    pub use_download_path: Option<bool>,
    pub download_path: Option<String>,
    pub auto_tmm: Option<bool>,
    pub stopped: Option<bool>,
    pub stop_condition: Option<StopCondition>,
    pub skip_checking: bool,
    pub add_to_queue_top: Option<bool>,
    pub content_layout: Option<ContentLayout>,
    pub sequential: bool,
    pub first_last_piece_priority: bool,
    pub forced: bool,
    /// bytes/s; None or <= 0 = unlimited.
    pub download_limit: Option<i64>,
    pub upload_limit: Option<i64>,
    /// -2 global, -1 unlimited.
    pub ratio_limit: Option<f64>,
    pub seeding_time_limit: Option<i64>,
    pub inactive_seeding_time_limit: Option<i64>,
    pub share_limit_action: Option<ShareLimitAction>,
    /// Per-file priorities in torrent file order; empty = all Normal.
    pub file_priorities: Vec<FilePriority>,
    /// Per-file renamed relative paths in torrent file order; empty = unchanged.
    pub file_paths: Vec<String>,
    /// Extra trackers to add.
    pub trackers: Vec<String>,
    pub url_seeds: Vec<String>,
    /// Cookie header for downloading a .torrent over HTTP.
    pub cookie: Option<String>,
}

/// A file inside torrent metadata (before the torrent is added).
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct MetadataFile {
    pub index: i64,
    pub path: String,
    #[serde(rename = "length")]
    pub size: i64,
}

/// Parsed torrent metadata for the "Add new torrent" dialog and
/// `/api/v2/torrents/parseMetadata`.
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentMetadataInfo {
    pub hash: String,
    pub infohash_v1: String,
    pub infohash_v2: String,
    pub name: String,
    pub has_metadata: bool,
    pub total_size: i64,
    pub piece_size: i64,
    pub pieces_num: i64,
    pub private: bool,
    pub comment: String,
    pub created_by: String,
    pub creation_date: i64,
    pub files: Vec<MetadataFile>,
    pub trackers: Vec<TrackerTierEntry>,
    pub url_seeds: Vec<String>,
    /// Torrent already exists in the session.
    pub is_duplicate: bool,
}

/// Filter for [`crate::App::torrents_filtered`] (`/api/v2/torrents/info` params).
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentQuery {
    pub status: StatusFilter,
    /// None = any; Some("") = uncategorized.
    pub category: Option<String>,
    /// None = any; Some("") = untagged.
    pub tag: Option<String>,
    /// Empty = all torrents.
    pub hashes: Vec<String>,
    pub private: Option<bool>,
}

/// Queue position change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum QueueMove {
    Up,
    Down,
    Top,
    Bottom,
}
