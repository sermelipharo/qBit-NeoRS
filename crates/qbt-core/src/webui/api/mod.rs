//! `/api/v2/<scope>/<action>` controllers. Each action parses its
//! parameters, calls the `App` facade and serializes the result with
//! qBittorrent's exact JSON names and status codes.

mod app;
mod creator;
mod rss;
mod search;
mod sync;
mod torrents;

use std::collections::VecDeque;
use std::sync::Arc;

use http::StatusCode;
use serde_json::{Value, json};

use super::http::{Params, Uploads};
use super::session::WebSession;
use super::sync::{MaindataSync, PeersSync};
use super::webapp::WebApp;
use crate::app::App;
use crate::error::QbtError;
use crate::model::{Category, CategoryDownloadPath, TorrentMetadataInfo};

pub(crate) use self::creator::CreatorExtras;
pub(crate) use self::sync::server_state_json;

/// Result of an action (qBittorrent `APIResult`).
#[derive(Debug)]
pub enum Reply {
    /// No `setResult` -> 204.
    NoContent,
    /// `text/plain; charset=UTF-8`, 200.
    Text(String),
    /// Compact JSON, 200 (or 202 with `Async`).
    Json(Value),
    Async(Value),
    /// Raw bytes with a MIME type and optional attachment file name.
    Bytes { data: Vec<u8>, mime: &'static str, filename: Option<String> },
}

impl Reply {
    pub fn ok() -> Self {
        Reply::Text(String::new())
    }
}

/// An error response: status + plain-text message (empty = status text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }
    pub fn bad_params(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, m)
    }
    pub fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "")
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "")
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, m)
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "")
    }
    pub fn not_found_msg(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, m)
    }
    pub fn endpoint_not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Endpoint does not exist")
    }
    pub fn method_not_allowed() -> Self {
        Self::new(StatusCode::METHOD_NOT_ALLOWED, "")
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, m)
    }
    pub fn bad_data(m: impl Into<String>) -> Self {
        Self::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, m)
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, m)
    }
    pub fn payload_too_large() -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "")
    }
}

/// Inner message of a facade error (without the variant prefix).
pub fn error_message(e: &QbtError) -> String {
    match e {
        QbtError::NotFound { message }
        | QbtError::InvalidArgument { message }
        | QbtError::Conflict { message }
        | QbtError::Io { message }
        | QbtError::Network { message }
        | QbtError::Engine { message }
        | QbtError::Unsupported { message }
        | QbtError::Other { message } => message.clone(),
    }
}

/// Default mapping of facade errors: unknown items -> 404, bad arguments ->
/// 400, features not implemented by the core yet -> 501, operation
/// failures -> 409 (qBittorrent's `Conflict`).
impl From<QbtError> for ApiError {
    fn from(e: QbtError) -> Self {
        let msg = error_message(&e);
        match e {
            QbtError::NotFound { .. } => ApiError::not_found(),
            QbtError::InvalidArgument { .. } => ApiError::bad_params(msg),
            QbtError::Unsupported { .. } => ApiError::new(StatusCode::NOT_IMPLEMENTED, msg),
            _ => ApiError::conflict(msg),
        }
    }
}

pub type ApiResult = Result<Reply, ApiError>;

/// Bulk actions skip unknown torrents silently.
pub(crate) fn skip_not_found(r: crate::error::QbtResult<()>) -> Result<(), ApiError> {
    match r {
        Ok(()) | Err(QbtError::NotFound { .. }) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

// ---- Endpoint table ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Methods {
    /// Only POST (anything else -> 405).
    PostOnly,
    /// GET and POST (HEAD is refused on the API).
    GetPost,
}

use Methods::{GetPost as GP, PostOnly as P};

/// Every WebAPI 2.15.1 endpoint with its method filter (features.md 6.7),
/// plus `app/getFreeSpaceAtPath`, which qBittorrent 5.2.4's own WebUI calls
/// (add-torrent dialog) although its backend does not implement it.
pub const ENDPOINTS: &[(&str, &str, Methods)] = &[
    ("auth", "login", P),
    ("auth", "logout", P),
    ("app", "version", GP),
    ("app", "webapiVersion", GP),
    ("app", "buildInfo", GP),
    ("app", "processInfo", GP),
    ("app", "shutdown", P),
    ("app", "preferences", GP),
    ("app", "setPreferences", P),
    ("app", "defaultSavePath", GP),
    ("app", "sendTestEmail", P),
    ("app", "getDirectoryContent", GP),
    ("app", "cookies", GP),
    ("app", "setCookies", P),
    ("app", "rotateAPIKey", P),
    ("app", "deleteAPIKey", P),
    ("app", "networkInterfaceList", GP),
    ("app", "networkInterfaceAddressList", GP),
    ("app", "getFreeSpaceAtPath", GP),
    ("clientdata", "load", GP),
    ("clientdata", "store", P),
    ("log", "main", GP),
    ("log", "peers", GP),
    ("sync", "maindata", GP),
    ("sync", "torrentPeers", GP),
    ("transfer", "info", GP),
    ("transfer", "speedLimitsMode", GP),
    ("transfer", "setSpeedLimitsMode", P),
    ("transfer", "toggleSpeedLimitsMode", P),
    ("transfer", "uploadLimit", GP),
    ("transfer", "downloadLimit", GP),
    ("transfer", "setUploadLimit", P),
    ("transfer", "setDownloadLimit", P),
    ("transfer", "banPeers", P),
    ("torrents", "count", GP),
    ("torrents", "info", GP),
    ("torrents", "properties", GP),
    ("torrents", "trackers", GP),
    ("torrents", "webseeds", GP),
    ("torrents", "files", GP),
    ("torrents", "pieceStates", GP),
    ("torrents", "pieceHashes", GP),
    ("torrents", "pieceAvailability", GP),
    ("torrents", "uploadLimit", GP),
    ("torrents", "downloadLimit", GP),
    ("torrents", "categories", GP),
    ("torrents", "tags", GP),
    ("torrents", "export", GP),
    ("torrents", "SSLParameters", GP),
    ("torrents", "saveMetadata", GP),
    ("torrents", "add", P),
    ("torrents", "fetchMetadata", P),
    ("torrents", "parseMetadata", P),
    ("torrents", "addTrackers", P),
    ("torrents", "editTracker", P),
    ("torrents", "removeTrackers", P),
    ("torrents", "addPeers", P),
    ("torrents", "addWebSeeds", P),
    ("torrents", "editWebSeed", P),
    ("torrents", "removeWebSeeds", P),
    ("torrents", "stop", P),
    ("torrents", "start", P),
    ("torrents", "filePrio", P),
    ("torrents", "setUploadLimit", P),
    ("torrents", "setDownloadLimit", P),
    ("torrents", "setShareLimits", P),
    ("torrents", "toggleSequentialDownload", P),
    ("torrents", "toggleFirstLastPiecePrio", P),
    ("torrents", "setSuperSeeding", P),
    ("torrents", "setForceStart", P),
    ("torrents", "delete", P),
    ("torrents", "increasePrio", P),
    ("torrents", "decreasePrio", P),
    ("torrents", "topPrio", P),
    ("torrents", "bottomPrio", P),
    ("torrents", "setLocation", P),
    ("torrents", "setSavePath", P),
    ("torrents", "setDownloadPath", P),
    ("torrents", "rename", P),
    ("torrents", "setComment", P),
    ("torrents", "setAutoManagement", P),
    ("torrents", "recheck", P),
    ("torrents", "reannounce", P),
    ("torrents", "setCategory", P),
    ("torrents", "createCategory", P),
    ("torrents", "editCategory", P),
    ("torrents", "removeCategories", P),
    ("torrents", "addTags", P),
    ("torrents", "setTags", P),
    ("torrents", "removeTags", P),
    ("torrents", "createTags", P),
    ("torrents", "deleteTags", P),
    ("torrents", "renameFile", P),
    ("torrents", "renameFolder", P),
    ("torrents", "setSSLParameters", P),
    ("rss", "addFolder", P),
    ("rss", "addFeed", P),
    ("rss", "setFeedURL", P),
    ("rss", "setFeedRefreshInterval", P),
    ("rss", "removeItem", P),
    ("rss", "moveItem", P),
    ("rss", "items", GP),
    ("rss", "markAsRead", P),
    ("rss", "refreshItem", P),
    ("rss", "setRule", P),
    ("rss", "renameRule", P),
    ("rss", "removeRule", P),
    ("rss", "rules", GP),
    ("rss", "matchingArticles", GP),
    ("search", "start", P),
    ("search", "stop", P),
    ("search", "status", GP),
    ("search", "results", GP),
    ("search", "delete", P),
    ("search", "downloadTorrent", P),
    ("search", "plugins", GP),
    ("search", "installPlugin", P),
    ("search", "uninstallPlugin", P),
    ("search", "enablePlugin", P),
    ("search", "updatePlugins", P),
    ("torrentcreator", "addTask", P),
    ("torrentcreator", "status", GP),
    ("torrentcreator", "torrentFile", GP),
    ("torrentcreator", "deleteTask", P),
];

/// Scopes served by per-session controllers (`auth` is shared).
pub const SESSION_SCOPES: &[&str] =
    &["app", "clientdata", "log", "torrentcreator", "rss", "search", "torrents", "transfer", "sync"];

pub fn method_filter(scope: &str, action: &str) -> Option<Methods> {
    ENDPOINTS.iter().find(|(s, a, _)| *s == scope && *a == action).map(|(_, _, m)| *m)
}

// ---- Per-session state -----------------------------------------------------------

/// Bound of per-session metadata caches and search job lists.
pub const MAX_SESSION_ITEMS: usize = 64;

/// Per-session controller state (qBittorrent creates controllers per session).
#[derive(Default)]
pub struct SessionData {
    pub maindata: MaindataSync,
    pub peers: PeersSync,
    /// Search jobs started by this session (oldest first).
    pub searches: VecDeque<i64>,
    /// `fetchMetadata` / `parseMetadata` results: source -> torrent ID.
    pub metadata_sources: VecDeque<(String, String)>,
    /// Torrent ID -> metadata (`None` while still downloading).
    pub metadata: VecDeque<(String, Option<TorrentMetadataInfo>)>,
}

impl SessionData {
    pub fn metadata_for(&self, id: &str) -> Option<&Option<TorrentMetadataInfo>> {
        self.metadata.iter().find(|(k, _)| k == id).map(|(_, v)| v)
    }

    pub fn set_metadata(&mut self, id: &str, info: Option<TorrentMetadataInfo>) {
        self.metadata.retain(|(k, _)| k != id);
        if self.metadata.len() >= MAX_SESSION_ITEMS {
            self.metadata.pop_front();
        }
        self.metadata.push_back((id.to_owned(), info));
    }

    pub fn remove_metadata(&mut self, id: &str) {
        self.metadata.retain(|(k, _)| k != id);
    }

    pub fn source_id(&self, source: &str) -> Option<String> {
        self.metadata_sources.iter().find(|(s, _)| s == source).map(|(_, id)| id.clone())
    }

    pub fn set_source(&mut self, source: &str, id: &str) {
        self.metadata_sources.retain(|(s, _)| s != source);
        if self.metadata_sources.len() >= MAX_SESSION_ITEMS {
            self.metadata_sources.pop_front();
        }
        self.metadata_sources.push_back((source.to_owned(), id.to_owned()));
    }

    pub fn remove_source(&mut self, source: &str) {
        self.metadata_sources.retain(|(s, _)| s != source);
    }
}

// ---- Call context ----------------------------------------------------------------

pub struct Call<'a> {
    pub web: &'a Arc<WebApp>,
    pub app: Arc<App>,
    pub session: Arc<WebSession>,
    pub params: Params,
    pub files: Uploads,
}

impl Call<'_> {
    /// `|`-separated IDs; the single value `all` means every torrent.
    pub fn torrent_ids(&self, key: &str) -> Vec<String> {
        let list: Vec<&str> = self.params.str(key).split('|').collect();
        if list.len() == 1 && list[0] == "all" {
            return self.app.torrents().into_iter().map(|t| t.hash).collect();
        }
        list.into_iter().filter(|s| !s.is_empty()).map(|s| s.trim().to_ascii_lowercase()).collect()
    }
}

/// Dispatches `scope/action` (the session is already established).
pub async fn dispatch(c: &mut Call<'_>, scope: &str, action: &str) -> ApiResult {
    match scope {
        "app" => app::app(c, action).await,
        "clientdata" => app::clientdata(c, action),
        "log" => app::log(c, action),
        "transfer" => app::transfer(c, action),
        "sync" => sync::dispatch(c, action).await,
        "torrents" => torrents::dispatch(c, action).await,
        "rss" => rss::dispatch(c, action),
        "search" => search::dispatch(c, action).await,
        "torrentcreator" => creator::dispatch(c, action),
        _ => Err(ApiError::endpoint_not_found()),
    }
}

// ---- Shared serializers ----------------------------------------------------------

/// WebAPI category object (`torrents/categories`, `sync/maindata`).
pub fn category_json(c: &Category) -> Value {
    let download_path = match &c.download_path {
        CategoryDownloadPath::Default => Value::Null,
        CategoryDownloadPath::Disabled => Value::Bool(false),
        CategoryDownloadPath::Path { path } => Value::String(path.clone()),
    };
    json!({
        "name": c.name,
        "savePath": c.save_path,
        "download_path": download_path,
        // The core's category model has no per-category share limits yet;
        // report "use global" like a default category.
        "ratio_limit": -2,
        "seeding_time_limit": -2,
        "inactive_seeding_time_limit": -2,
        "share_limit_action": "Default",
    })
}

/// `{infohash_v1, infohash_v2, hash}` of a metadata request in progress.
pub fn info_hash_json(v1: &str, v2: &str, id: &str) -> Value {
    json!({"infohash_v1": v1, "infohash_v2": v2, "hash": id})
}

/// Full `fetchMetadata` / `parseMetadata` info object.
pub fn metadata_json(m: &TorrentMetadataInfo) -> Value {
    let files: Vec<Value> = m.files.iter().map(|f| json!({"path": f.path, "length": f.size})).collect();
    let length: i64 = m.files.iter().map(|f| f.size).sum();
    json!({
        "infohash_v1": m.infohash_v1,
        "infohash_v2": m.infohash_v2,
        "hash": m.hash,
        "info": {
            "files": files,
            "length": if m.files.is_empty() { m.total_size } else { length },
            "name": m.name,
            "piece_length": m.piece_size,
            "pieces_num": m.pieces_num,
            "private": m.private,
        },
        "trackers": m.trackers.iter().map(|t| json!({"url": t.url, "tier": t.tier})).collect::<Vec<_>>(),
        "webseeds": m.url_seeds,
        "created_by": m.created_by,
        "creation_date": m.creation_date,
        "comment": m.comment,
    })
}

/// Info hashes parsed from a magnet link or a bare hash:
/// (v1 hex, v2 hex, torrent ID).
pub fn parse_info_hash(source: &str) -> Option<(String, String, String)> {
    let s = source.trim();
    let is_hex = |x: &str, n: usize| x.len() == n && x.bytes().all(|b| b.is_ascii_hexdigit());
    let from_base32 = |x: &str| -> Option<String> {
        if x.len() != 32 {
            return None;
        }
        let mut bits: u64 = 0;
        let mut nbits = 0;
        let mut out = Vec::with_capacity(20);
        for c in x.bytes() {
            let v = match c.to_ascii_uppercase() {
                b'A'..=b'Z' => c.to_ascii_uppercase() - b'A',
                b'2'..=b'7' => c - b'2' + 26,
                _ => return None,
            };
            bits = (bits << 5) | u64::from(v);
            nbits += 5;
            if nbits >= 8 {
                nbits -= 8;
                out.push((bits >> nbits) as u8);
                bits &= (1 << nbits) - 1;
            }
        }
        Some(out.iter().map(|b| format!("{b:02x}")).collect())
    };
    let mut v1 = String::new();
    let mut v2 = String::new();
    if let Some(query) = s.strip_prefix("magnet:?").or_else(|| s.strip_prefix("MAGNET:?")) {
        for part in query.split('&') {
            let Some((k, v)) = part.split_once('=') else { continue };
            if !(k == "xt" || k.starts_with("xt.")) {
                continue;
            }
            let v = super::http::percent_decode_str(v);
            if let Some(h) = v.strip_prefix("urn:btih:") {
                if is_hex(h, 40) {
                    v1 = h.to_ascii_lowercase();
                } else if let Some(hex) = from_base32(h) {
                    v1 = hex;
                }
            } else if let Some(h) = v.strip_prefix("urn:btmh:1220")
                && is_hex(h, 64)
            {
                v2 = h.to_ascii_lowercase();
            }
        }
    } else if is_hex(s, 40) {
        v1 = s.to_ascii_lowercase();
    } else if is_hex(s, 64) {
        v2 = s.to_ascii_lowercase();
    } else if let Some(hex) = from_base32(s) {
        v1 = hex;
    }
    if v1.is_empty() && v2.is_empty() {
        return None;
    }
    let id = if v2.is_empty() { v1.clone() } else { v2[..40].to_owned() };
    Some((v1, v2, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_table() {
        assert_eq!(ENDPOINTS.len(), 124);
        let post = ENDPOINTS.iter().filter(|e| e.2 == Methods::PostOnly).count();
        let get = ENDPOINTS.len() - post;
        assert_eq!(post, 80);
        assert_eq!(get, 44);
        assert_eq!(method_filter("torrents", "SSLParameters"), Some(Methods::GetPost));
        assert_eq!(method_filter("torrents", "ssLParameters"), None);
        assert_eq!(method_filter("rss", "setFeedRefreshInterval"), Some(Methods::PostOnly));
        let mut seen = std::collections::HashSet::new();
        for (s, a, _) in ENDPOINTS {
            assert!(seen.insert((s, a)), "duplicate {s}/{a}");
            assert!(*s == "auth" || SESSION_SCOPES.contains(s));
        }
    }

    #[test]
    fn info_hashes() {
        let h = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(parse_info_hash(h), Some((h.into(), "".into(), h.into())));
        assert_eq!(parse_info_hash(&h.to_uppercase()).unwrap().2, h);
        let m = format!("magnet:?xt=urn:btih:{h}&dn=x");
        assert_eq!(parse_info_hash(&m).unwrap().0, h);
        let v2 = "a".repeat(64);
        let m = format!("magnet:?xt=urn:btih:{h}&xt=urn:btmh:1220{v2}");
        let (a, b, id) = parse_info_hash(&m).unwrap();
        assert_eq!((a.as_str(), b.as_str(), id.as_str()), (h, v2.as_str(), &v2[..40]));
        // base32 (32 chars) of 20 zero bytes
        assert_eq!(parse_info_hash(&"A".repeat(32)).unwrap().0, "0".repeat(40));
        assert!(parse_info_hash("http://x/y.torrent").is_none());
        assert!(parse_info_hash("magnet:?dn=only").is_none());
        assert!(parse_info_hash("").is_none());
    }

    #[test]
    fn error_mapping() {
        assert_eq!(ApiError::from(QbtError::not_found("x")).status, StatusCode::NOT_FOUND);
        assert_eq!(ApiError::from(QbtError::invalid("bad")).message, "bad");
        assert_eq!(ApiError::from(QbtError::conflict("c")).status, StatusCode::CONFLICT);
        assert_eq!(ApiError::from(QbtError::unsupported("u")).status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(ApiError::from(QbtError::io("i")).status, StatusCode::CONFLICT);
    }
}
