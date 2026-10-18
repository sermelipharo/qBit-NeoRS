//! `torrents/*`.

use std::cmp::Ordering;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{
    ApiError, ApiResult, Call, Reply, category_json, info_hash_json, metadata_json, parse_info_hash, skip_not_found,
};
use crate::QbtError;
use crate::model::*;
use crate::webui::http::{parse_int32, percent_decode_str};
use crate::webui::security::sanitize_for_log;

/// How long a background metadata fetch / URL download may run.
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Keys only present in `sync/maindata` torrent objects.
const MAINDATA_ONLY: [&str; 3] = ["has_tracker_warning", "has_tracker_error", "has_other_announce_error"];

pub async fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "count" => Ok(Reply::Text(c.app.torrent_count().to_string())),
        "info" => info(c),
        "properties" => properties(c),
        "trackers" => trackers(c),
        "webseeds" => {
            c.params.require(&["hash"])?;
            let seeds = c.app.web_seeds(hash(c, "hash"))?;
            Ok(Reply::Json(json!(seeds.iter().map(|s| json!({"url": s.url})).collect::<Vec<_>>())))
        }
        "files" => files(c),
        "pieceStates" | "pieceHashes" | "pieceAvailability" => {
            c.params.require(&["hash"])?;
            let h = hash(c, "hash");
            Ok(Reply::Json(match action {
                "pieceStates" => json!(c.app.piece_states(h)?),
                "pieceHashes" => json!(c.app.piece_hashes(h)?),
                _ => json!(c.app.piece_availability(h)?),
            }))
        }
        "uploadLimit" | "downloadLimit" => limits(c, action == "uploadLimit"),
        "categories" => {
            let mut m = Map::new();
            for cat in c.app.categories() {
                m.insert(cat.name.clone(), category_json(&cat));
            }
            Ok(Reply::Json(Value::Object(m)))
        }
        "tags" => Ok(Reply::Json(json!(c.app.tags()))),
        "export" => export(c),
        "SSLParameters" => {
            c.params.require(&["hash"])?;
            let h = hash(c, "hash");
            c.app.torrent(h.clone())?;
            let p = c.web.ssl_parameters(&h);
            Ok(Reply::Json(json!({
                "ssl_certificate": p.certificate, "ssl_private_key": p.private_key, "ssl_dh_params": p.dh_params,
            })))
        }
        "setSSLParameters" => set_ssl(c),
        "saveMetadata" => save_metadata(c).await,
        "fetchMetadata" => fetch_metadata(c).await,
        "parseMetadata" => parse_metadata(c).await,
        "add" => add(c).await,
        "addTrackers" => {
            c.params.require(&["hash", "urls"])?;
            let entries = parse_tracker_entries(c.params.str("urls"));
            for id in c.torrent_ids("hash") {
                skip_not_found(c.app.add_trackers(id, entries.clone()))?;
            }
            Ok(Reply::NoContent)
        }
        "editTracker" => edit_tracker(c).await,
        "removeTrackers" => {
            c.params.require(&["hash", "urls"])?;
            let key = if c.params.str("hash") == "*" { None } else { Some("hash") };
            let ids = match key {
                Some(k) => c.torrent_ids(k),
                None => c.app.torrents().into_iter().map(|t| t.hash).collect(),
            };
            let urls: Vec<String> =
                c.params.str("urls").split('|').filter(|s| !s.is_empty()).map(percent_decode_str).collect();
            for id in ids {
                skip_not_found(c.app.remove_trackers(id, urls.clone()))?;
            }
            Ok(Reply::NoContent)
        }
        "addPeers" => add_peers(c),
        "addWebSeeds" | "removeWebSeeds" => {
            c.params.require(&["hash", "urls"])?;
            let h = hash(c, "hash");
            c.app.web_seeds(h.clone())?;
            let mut urls = Vec::new();
            for u in c.params.str("urls").split('|').filter(|s| !s.is_empty()) {
                urls.push(validate_web_seed(u)?);
            }
            if action == "addWebSeeds" {
                c.app.add_web_seeds(h, urls)?;
            } else {
                c.app.remove_web_seeds(h, urls)?;
            }
            Ok(Reply::ok())
        }
        "editWebSeed" => {
            c.params.require(&["hash", "origUrl", "newUrl"])?;
            let h = hash(c, "hash");
            let existing = c.app.web_seeds(h.clone())?;
            let orig = validate_web_seed(c.params.str("origUrl"))?;
            let new = validate_web_seed(c.params.str("newUrl"))?;
            if orig != new {
                if !existing.iter().any(|s| s.url == orig) {
                    return Err(ApiError::conflict(format!("\"{orig}\" is not an existing URL")));
                }
                c.app.edit_web_seed(h, orig, new)?;
            }
            Ok(Reply::ok())
        }
        "stop" | "start" | "recheck" | "reannounce" => {
            c.params.require(&["hashes"])?;
            let ids = c.torrent_ids("hashes");
            // `urls` (reannounce to specific trackers) is not supported by
            // the core yet: every tracker is reannounced.
            let r = match action {
                "stop" => c.app.stop_torrents(ids).await,
                "start" => c.app.start_torrents(ids).await,
                "recheck" => c.app.recheck_torrents(ids).await,
                _ => c.app.reannounce_torrents(ids).await,
            };
            skip_not_found(r)?;
            Ok(Reply::ok())
        }
        "filePrio" => file_prio(c),
        "setUploadLimit" | "setDownloadLimit" => {
            c.params.require(&["hashes", "limit"])?;
            let mut limit = c.params.i64("limit");
            if limit == 0 {
                limit = -1;
            }
            let ids = c.torrent_ids("hashes");
            let r = if action == "setUploadLimit" {
                c.app.set_upload_limit(ids, limit)
            } else {
                c.app.set_download_limit(ids, limit)
            };
            skip_not_found(r)?;
            Ok(Reply::ok())
        }
        "setShareLimits" => {
            c.params.require(&["hashes", "ratioLimit", "seedingTimeLimit", "inactiveSeedingTimeLimit", "shareLimitAction"])?;
            let coerce_f = |v: f64| if v < -2.0 { -1.0 } else { v };
            let coerce_i = |v: i64| if v < -2 { -1 } else { v };
            let ratio = coerce_f(c.params.f64_opt("ratioLimit").unwrap_or(0.0));
            let seeding = coerce_i(c.params.i64("seedingTimeLimit"));
            let inactive = coerce_i(c.params.i64("inactiveSeedingTimeLimit"));
            let action = share_limit_action(c.params.str("shareLimitAction"));
            skip_not_found(c.app.set_share_limits(c.torrent_ids("hashes"), ratio, seeding, inactive, action))?;
            Ok(Reply::ok())
        }
        "toggleSequentialDownload" | "toggleFirstLastPiecePrio" => {
            c.params.require(&["hashes"])?;
            let torrents = selected(c, "hashes");
            let seq = action == "toggleSequentialDownload";
            let (on, off): (Vec<_>, Vec<_>) =
                torrents.into_iter().partition(|t| if seq { t.seq_dl } else { t.f_l_piece_prio });
            for (list, value) in [(on, false), (off, true)] {
                let ids: Vec<String> = list.into_iter().map(|t| t.hash).collect();
                if ids.is_empty() {
                    continue;
                }
                let r = if seq {
                    c.app.set_sequential_download(ids, value)
                } else {
                    c.app.set_first_last_piece_priority(ids, value)
                };
                skip_not_found(r)?;
            }
            Ok(Reply::ok())
        }
        "setSuperSeeding" | "setForceStart" | "setAutoManagement" => {
            let key = if action == "setAutoManagement" { "enable" } else { "value" };
            c.params.require(&["hashes", key])?;
            let value = c.params.bool_or(key, false);
            let ids = c.torrent_ids("hashes");
            let r = match action {
                "setSuperSeeding" => c.app.set_super_seeding(ids, value),
                "setAutoManagement" => c.app.set_auto_management(ids, value),
                _ => c.app.set_force_start(ids, value).await,
            };
            skip_not_found(r)?;
            Ok(Reply::ok())
        }
        "delete" => {
            c.params.require(&["hashes", "deleteFiles"])?;
            let delete_files = c.params.bool_or("deleteFiles", false);
            skip_not_found(c.app.remove_torrents(c.torrent_ids("hashes"), delete_files).await)?;
            Ok(Reply::ok())
        }
        "increasePrio" | "decreasePrio" | "topPrio" | "bottomPrio" => {
            c.params.require(&["hashes"])?;
            if !c.app.preferences().bittorrent.queueing_enabled {
                return Err(ApiError::conflict("Torrent queueing must be enabled"));
            }
            // `all` is not special here.
            let ids: Vec<String> = c.params.str("hashes").split('|').map(|s| s.trim().to_ascii_lowercase()).collect();
            let r = match action {
                "increasePrio" => c.app.queue_increase(ids),
                "decreasePrio" => c.app.queue_decrease(ids),
                "topPrio" => c.app.queue_top(ids),
                _ => c.app.queue_bottom(ids),
            };
            skip_not_found(r)?;
            Ok(Reply::ok())
        }
        "setLocation" => set_location(c).await,
        "setSavePath" | "setDownloadPath" => set_path(c, action == "setSavePath").await,
        "rename" => {
            c.params.require(&["hash", "name"])?;
            let name = c.params.str("name").trim().to_owned();
            if name.is_empty() {
                return Err(ApiError::conflict("Incorrect torrent name"));
            }
            let name = name.replace("\r\n", " ").replace(['\r', '\n'], " ");
            c.app.rename_torrent(hash(c, "hash"), name)?;
            Ok(Reply::ok())
        }
        "setComment" => {
            c.params.require(&["hashes", "comment"])?;
            let comment = c.params.str("comment").trim().to_owned();
            skip_not_found(c.app.set_comment(c.torrent_ids("hashes"), comment))?;
            Ok(Reply::NoContent)
        }
        "setCategory" => {
            c.params.require(&["hashes", "category"])?;
            let category = c.params.str("category").to_owned();
            if !category.is_empty() && !c.app.categories().iter().any(|x| x.name == category) {
                return Err(ApiError::conflict("Incorrect category name"));
            }
            match c.app.set_category(c.torrent_ids("hashes"), category) {
                Ok(()) | Err(QbtError::NotFound { .. }) => Ok(Reply::ok()),
                Err(QbtError::InvalidArgument { .. } | QbtError::Conflict { .. }) => {
                    Err(ApiError::conflict("Incorrect category name"))
                }
                Err(e) => Err(e.into()),
            }
        }
        "createCategory" | "editCategory" => edit_category(c, action == "createCategory"),
        "removeCategories" => {
            c.params.require(&["categories"])?;
            let names: Vec<String> = c.params.str("categories").split('\n').map(str::to_owned).collect();
            skip_not_found(c.app.remove_categories(names))?;
            Ok(Reply::ok())
        }
        "addTags" | "setTags" | "removeTags" => {
            let tags_required = action != "removeTags";
            c.params.require(if tags_required { &["hashes", "tags"] } else { &["hashes"] })?;
            let tags = split_tags(c.params.str("tags"));
            let ids = c.torrent_ids("hashes");
            let r = match action {
                "addTags" if tags.is_empty() => Ok(()),
                "addTags" => c.app.add_tags(ids, tags),
                "setTags" => c.app.set_tags(ids, tags),
                _ => c.app.remove_tags(ids, tags),
            };
            skip_not_found(r)?;
            Ok(if action == "setTags" { Reply::NoContent } else { Reply::ok() })
        }
        "createTags" | "deleteTags" => {
            c.params.require(&["tags"])?;
            let tags = split_tags(c.params.str("tags"));
            if !tags.is_empty() {
                let r = if action == "createTags" { c.app.create_tags(tags) } else { c.app.delete_tags(tags) };
                skip_not_found(r)?;
            }
            Ok(Reply::ok())
        }
        "renameFile" | "renameFolder" => rename_path(c, action == "renameFile").await,
        _ => Err(ApiError::endpoint_not_found()),
    }
}

/// Torrent ID parameter (normalised).
fn hash(c: &Call<'_>, key: &str) -> String {
    c.params.str(key).trim().to_ascii_lowercase()
}

fn split_tags(s: &str) -> Vec<String> {
    s.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).collect()
}

/// Torrents selected by a `|`-separated (or `all`) parameter.
fn selected(c: &Call<'_>, key: &str) -> Vec<TorrentInfo> {
    let ids = c.torrent_ids(key);
    if ids.is_empty() {
        return Vec::new();
    }
    c.app.torrents_filtered(TorrentQuery { hashes: ids, ..Default::default() })
}

fn status_filter(s: &str) -> StatusFilter {
    match s {
        "downloading" => StatusFilter::Downloading,
        "seeding" => StatusFilter::Seeding,
        "completed" => StatusFilter::Completed,
        "stopped" => StatusFilter::Stopped,
        "running" => StatusFilter::Running,
        "active" => StatusFilter::Active,
        "inactive" => StatusFilter::Inactive,
        "stalled" => StatusFilter::Stalled,
        "stalled_uploading" => StatusFilter::StalledUploading,
        "stalled_downloading" => StatusFilter::StalledDownloading,
        "checking" => StatusFilter::Checking,
        "moving" => StatusFilter::Moving,
        "errored" => StatusFilter::Errored,
        _ => StatusFilter::All,
    }
}

fn share_limit_action(s: &str) -> ShareLimitAction {
    match s {
        "Stop" => ShareLimitAction::Stop,
        "Remove" => ShareLimitAction::Remove,
        "RemoveWithContent" => ShareLimitAction::RemoveWithContent,
        "EnableSuperSeeding" => ShareLimitAction::EnableSuperSeeding,
        _ => ShareLimitAction::Default,
    }
}

/// `torrents/info` object (no maindata-only keys, 0 = unlimited limits).
pub fn torrent_json(t: &TorrentInfo) -> Map<String, Value> {
    let mut m = match serde_json::to_value(t) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for k in MAINDATA_ONLY {
        m.shift_remove(k);
    }
    for k in ["dl_limit", "up_limit"] {
        if let Some(v) = m.get_mut(k)
            && v.as_i64().is_some_and(|x| x < 0)
        {
            *v = json!(0);
        }
    }
    m
}

fn file_json(f: &TorrentFile, keep_is_seed: bool) -> Value {
    let mut m = match serde_json::to_value(f) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    if !keep_is_seed {
        m.shift_remove("is_seed");
    }
    Value::Object(m)
}

/// Real trackers as `torrents/trackers` rows (pseudo rows have tier < 0).
fn tracker_rows(entries: Vec<TrackerEntry>, include_pseudo: bool) -> Vec<Value> {
    entries
        .into_iter()
        .filter(|e| include_pseudo || e.tier >= 0)
        .filter_map(|e| {
            let pseudo = e.tier < 0;
            let Ok(Value::Object(mut m)) = serde_json::to_value(&e) else { return None };
            if pseudo {
                for k in ["updating", "next_announce", "min_announce", "endpoints"] {
                    m.shift_remove(k);
                }
            }
            Some(Value::Object(m))
        })
        .collect()
}

fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i.cmp(&j),
            _ => x.as_f64().unwrap_or(0.0).partial_cmp(&y.as_f64().unwrap_or(0.0)).unwrap_or(Ordering::Equal),
        },
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// `QList::mid(pos, len)` semantics.
fn qt_mid(size: usize, pos: i64, len: i64) -> std::ops::Range<usize> {
    let size_i = size as i64;
    if pos > size_i {
        return 0..0;
    }
    let (mut pos, mut len) = (pos, len);
    if pos < 0 {
        if len < 0 || len + pos >= size_i {
            return 0..size;
        }
        if len + pos <= 0 {
            return 0..0;
        }
        len += pos;
        pos = 0;
    } else if len < 0 || len > size_i - pos {
        len = size_i - pos;
    }
    pos as usize..(pos + len) as usize
}

/// Sorting, reversing and paging of `torrents/info`.
fn sort_and_page(
    mut items: Vec<Map<String, Value>>,
    sort: &str,
    reverse: bool,
    limit: i64,
    offset: i64,
) -> Result<Vec<Map<String, Value>>, ApiError> {
    if items.is_empty() {
        return Ok(items);
    }
    if !sort.is_empty() {
        if !items[0].contains_key(sort) {
            return Err(ApiError::bad_params("'sort' parameter is invalid"));
        }
        items.sort_by(|a, b| {
            let (x, y) = (a.get(sort).unwrap_or(&Value::Null), b.get(sort).unwrap_or(&Value::Null));
            if reverse { compare(y, x) } else { compare(x, y) }
        });
    }
    let size = items.len() as i64;
    let offset = if offset < 0 { size + offset } else { offset };
    let limit = if limit <= 0 { -1 } else { limit };
    if limit > 0 || offset > 0 {
        let r = qt_mid(items.len(), offset, limit);
        items = items.drain(r).collect();
    }
    Ok(items)
}

fn info(c: &mut Call<'_>) -> ApiResult {
    let p = &c.params;
    let hashes: Vec<String> =
        p.str("hashes").split('|').filter(|s| !s.is_empty()).map(|s| s.trim().to_ascii_lowercase()).collect();
    let query = TorrentQuery {
        status: status_filter(p.str("filter")),
        category: p.get("category").map(str::to_owned),
        tag: p.get("tag").map(|t| t.trim().to_owned()),
        hashes,
        private: p.bool_opt("private"),
    };
    let include_files = p.bool_or("includeFiles", false);
    let include_trackers = p.bool_or("includeTrackers", false);
    let mut items = Vec::new();
    for t in c.app.torrents_filtered(query) {
        let mut m = torrent_json(&t);
        if include_files && t.has_metadata {
            let files = c.app.files(t.hash.clone(), Vec::new()).unwrap_or_default();
            m.insert("files".into(), Value::Array(files.iter().map(|f| file_json(f, false)).collect()));
        }
        if include_trackers {
            let trackers = c.app.trackers(t.hash.clone()).unwrap_or_default();
            m.insert("trackers".into(), Value::Array(tracker_rows(trackers, false)));
        }
        items.push(m);
    }
    let items = sort_and_page(items, p.str("sort"), p.bool_or("reverse", false), p.int("limit"), p.int("offset"))?;
    Ok(Reply::Json(Value::Array(items.into_iter().map(Value::Object).collect())))
}

fn properties(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash"])?;
    let p = c.app.properties(hash(c, "hash"))?;
    let mut m = match serde_json::to_value(&p) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    for k in ["dl_limit", "up_limit"] {
        if let Some(v) = m.get_mut(k)
            && v.as_i64().is_some_and(|x| x <= 0)
        {
            *v = json!(-1);
        }
    }
    for (k, v) in [("share_ratio", p.share_ratio), ("popularity", p.popularity)] {
        if v >= MAX_RATIO {
            m.insert(k.into(), json!(-1));
        }
    }
    m.insert("is_private".into(), json!(p.private.unwrap_or(false)));
    Ok(Reply::Json(Value::Object(m)))
}

fn trackers(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash"])?;
    let entries = c.app.trackers(hash(c, "hash"))?;
    Ok(Reply::Json(Value::Array(tracker_rows(entries, true))))
}

fn files(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash"])?;
    let all = c.app.files(hash(c, "hash"), Vec::new())?;
    if all.is_empty() {
        return Ok(Reply::Json(json!([])));
    }
    let selected: Vec<&TorrentFile> = match c.params.get("indexes") {
        None => all.iter().collect(),
        Some(list) => {
            let mut out = Vec::new();
            for s in list.split('|') {
                let idx = parse_int32(s).filter(|i| *i >= 0);
                let Some(idx) = idx else {
                    return Err(ApiError::conflict(format!("\"{s}\" is not a valid file index.")));
                };
                let f = all.get(idx as usize).ok_or_else(|| ApiError::conflict(format!("Index {s} is out of bounds.")))?;
                out.push(f);
            }
            out
        }
    };
    let list: Vec<Value> = selected.iter().enumerate().map(|(i, f)| file_json(f, i == 0)).collect();
    Ok(Reply::Json(Value::Array(list)))
}

fn limits(c: &mut Call<'_>, upload: bool) -> ApiResult {
    c.params.require(&["hashes"])?;
    let ids: Vec<&str> = c.params.str("hashes").split('|').collect();
    let query = TorrentQuery {
        hashes: ids.iter().filter(|s| !s.is_empty()).map(|s| s.trim().to_ascii_lowercase()).collect(),
        ..Default::default()
    };
    let torrents = if query.hashes.is_empty() { Vec::new() } else { c.app.torrents_filtered(query) };
    let mut m = Map::new();
    for id in ids {
        let lower = id.trim().to_ascii_lowercase();
        let limit = torrents
            .iter()
            .find(|t| t.hash == lower)
            .map(|t| if upload { t.up_limit } else { t.dl_limit }.max(0))
            .unwrap_or(-1);
        m.insert(id.to_owned(), json!(limit));
    }
    Ok(Reply::Json(Value::Object(m)))
}

/// Only hex torrent IDs end up in a Content-Disposition file name.
fn safe_file_stem(id: &str) -> String {
    let s: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).take(64).collect();
    if s.is_empty() { "torrent".into() } else { s }
}

fn export(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash"])?;
    let h = hash(c, "hash");
    let data = c.app.export_torrent(h.clone()).map_err(|e| match e {
        QbtError::NotFound { .. } => ApiError::not_found(),
        QbtError::Unsupported { .. } => e.into(),
        other => ApiError::conflict(format!("Unable to export torrent file. Error: {}", super::error_message(&other))),
    })?;
    Ok(Reply::Bytes { data, mime: "application/x-bittorrent", filename: Some(format!("{}.torrent", safe_file_stem(&h))) })
}

fn set_ssl(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash", "ssl_certificate", "ssl_private_key", "ssl_dh_params"])?;
    let h = hash(c, "hash");
    c.app.torrent(h.clone())?;
    let params = crate::webui::webapp::SslParameters {
        certificate: c.params.str("ssl_certificate").to_owned(),
        private_key: c.params.str("ssl_private_key").to_owned(),
        dh_params: c.params.str("ssl_dh_params").to_owned(),
    };
    if !params.is_valid() {
        return Err(ApiError::bad_data(""));
    }
    c.web.set_ssl_parameters(&h, params);
    Ok(Reply::ok())
}

fn is_remote_url(s: &str) -> bool {
    let l = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("ftp://")
}

fn plugin_exists(c: &Call<'_>, name: &str) -> bool {
    c.app.search_plugins().iter().any(|p| p.name == name)
}

async fn save_metadata(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["source"])?;
    let raw = c.params.str("source").trim();
    if raw.is_empty() {
        return Err(ApiError::bad_params("Must specify URI or hash"));
    }
    let source = percent_decode_str(raw);
    let id = {
        let d = c.session.data.lock().await;
        d.source_id(&source).filter(|s| !s.is_empty()).or_else(|| parse_info_hash(&source).map(|x| x.2))
    };
    let Some(id) = id else { return Err(ApiError::not_found()) };
    let available = matches!(c.session.data.lock().await.metadata_for(&id), Some(Some(_)));
    if !available {
        return Err(ApiError::conflict("Metadata is not yet available"));
    }
    let data = c.app.save_fetched_metadata(id.clone()).map_err(|e| match e {
        QbtError::Unsupported { .. } => e.into(),
        other => ApiError::conflict(format!("Unable to export torrent metadata. Error: {}", super::error_message(&other))),
    })?;
    Ok(Reply::Bytes { data, mime: "application/x-bittorrent", filename: Some(format!("{}.torrent", safe_file_stem(&id))) })
}

async fn fetch_metadata(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["source"])?;
    let raw = c.params.str("source").trim().to_owned();
    if raw.is_empty() {
        return Err(ApiError::bad_params("Must specify URI or hash"));
    }
    let downloader = c.params.str("downloader");
    if !downloader.is_empty() && !plugin_exists(c, downloader) {
        return Err(ApiError::bad_params("downloader must be a valid search plugin"));
    }
    let source = percent_decode_str(&raw);
    let parsed = parse_info_hash(&source);
    let mut data = c.session.data.lock().await;
    let cached_id = data.source_id(&source);
    let id = parsed.as_ref().map(|p| p.2.clone()).or_else(|| cached_id.clone().filter(|s| !s.is_empty()));

    if let Some(id) = id {
        match data.metadata_for(&id) {
            Some(Some(info)) => return Ok(Reply::Json(metadata_json(info))),
            Some(None) => {
                let (v1, v2) = parsed.map(|p| (p.0, p.1)).unwrap_or_default();
                return Ok(Reply::Async(info_hash_json(&v1, &v2, &id)));
            }
            None => {}
        }
        let (v1, v2) = parsed.clone().map(|p| (p.0, p.1)).unwrap_or_default();
        data.set_metadata(&id, None);
        drop(data);
        spawn_fetch(c, TorrentSource::Url { url: source.clone() }, Some(id.clone()), None);
        return Ok(Reply::Async(info_hash_json(&v1, &v2, &id)));
    }
    if is_remote_url(&source) {
        if cached_id.is_none() {
            // Empty ID marks a download in progress.
            data.set_source(&source, "");
            drop(data);
            spawn_fetch(c, TorrentSource::Url { url: source.clone() }, None, Some(source.clone()));
        }
        return Ok(Reply::Async(json!({})));
    }
    Err(ApiError::bad_params(format!("Unable to parse '{source}'")))
}

/// Fetches metadata in the background and stores it in the session cache.
fn spawn_fetch(c: &Call<'_>, src: TorrentSource, id: Option<String>, source_key: Option<String>) {
    let app = c.app.clone();
    let session = c.session.clone();
    let web = c.web.clone();
    c.web.runtime().spawn(async move {
        let r = tokio::time::timeout(BACKGROUND_TIMEOUT, app.fetch_metadata(src)).await;
        let mut data = session.data.lock().await;
        match r {
            Ok(Ok(info)) => {
                if let Some(id) = &id
                    && *id != info.hash
                {
                    data.remove_metadata(id);
                }
                if let Some(s) = &source_key {
                    data.set_source(s, &info.hash);
                }
                let hash = info.hash.clone();
                data.set_metadata(&hash, Some(info));
            }
            Ok(Err(e)) => {
                if let Some(id) = &id {
                    data.remove_metadata(id);
                }
                if let Some(s) = &source_key {
                    data.remove_source(s);
                }
                web.log_warning(format!("Unable to download metadata. Error: {}", super::error_message(&e)));
            }
            Err(_) => {
                if let Some(id) = &id {
                    data.remove_metadata(id);
                }
                if let Some(s) = &source_key {
                    data.remove_source(s);
                }
            }
        }
    });
}

fn file_too_large(c: &Call<'_>, len: usize) -> bool {
    let limit = c.app.preferences().advanced.torrent_file_size_limit;
    limit > 0 && len as i64 > limit
}

async fn parse_metadata(c: &mut Call<'_>) -> ApiResult {
    if c.files.is_empty() {
        return Err(ApiError::bad_params("Must specify torrent file(s)"));
    }
    let files = std::mem::take(&mut c.files);
    let mut out = Vec::with_capacity(files.len());
    for (name, data) in files {
        if file_too_large(c, data.len()) {
            return Err(ApiError::bad_data(format!("'{name}' is not a valid torrent file.")));
        }
        match c.app.fetch_metadata(TorrentSource::Bytes { data }).await {
            Ok(info) => {
                out.push(metadata_json(&info));
                let hash = info.hash.clone();
                c.session.data.lock().await.set_metadata(&hash, Some(info));
            }
            Err(QbtError::InvalidArgument { .. }) => {
                return Err(ApiError::bad_data(format!("'{name}' is not a valid torrent file.")));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Reply::Json(Value::Array(out)))
}

fn parse_file_priority(s: &str) -> Result<FilePriority, ApiError> {
    let v = parse_int32(s).ok_or_else(|| ApiError::bad_params("Priority must be an integer"))?;
    match v {
        0 | 1 | 6 | 7 => Ok(FilePriority::from_i32(v as i32).unwrap_or_default()),
        _ => Err(ApiError::bad_params("Priority is not valid")),
    }
}

async fn add(c: &mut Call<'_>) -> ApiResult {
    let p = &c.params;
    let urls: Vec<String> = p.str("urls").split('\n').map(str::trim).filter(|u| !u.is_empty()).map(str::to_owned).collect();
    let file_prios_param: Vec<&str> = p.str("filePriorities").split(',').filter(|s| !s.is_empty()).collect();
    let mut file_prios = Vec::new();
    if !file_prios_param.is_empty() {
        if urls.len() > 1 {
            return Err(ApiError::bad_params("Cannot specify filePriorities when adding multiple torrents"));
        }
        if !c.files.is_empty() {
            return Err(ApiError::bad_params("Cannot specify filePriorities when uploading torrent files"));
        }
        for s in file_prios_param {
            file_prios.push(parse_file_priority(s)?);
        }
    }
    let downloader = p.str("downloader").to_owned();
    if !downloader.is_empty() && !plugin_exists(c, &downloader) {
        return Err(ApiError::bad_params("`downloader` must be a valid search plugin"));
    }
    let non_empty = |k: &str| Some(p.str(k).trim().to_owned()).filter(|s| !s.is_empty());
    let int_or = |k: &str, d: i64| parse_int32(p.str(k)).unwrap_or(d);
    let base = AddTorrentParams {
        name: non_empty("rename"),
        category: Some(p.str("category").to_owned()).filter(|s| !s.is_empty()),
        tags: split_tags(p.str("tags")),
        save_path: non_empty("savepath"),
        use_download_path: p.bool_opt("useDownloadPath"),
        download_path: non_empty("downloadPath"),
        auto_tmm: p.bool_opt("autoTMM"),
        // `paused` and `root_folder` are the names qBittorrent 4.x used;
        // older clients (FlexGet, scripts) still send them.
        stopped: p.bool_opt("stopped").or_else(|| p.bool_opt("paused")),
        stop_condition: Some(p.str("stopCondition")).filter(|s| !s.is_empty()).map(|s| match s {
            "MetadataReceived" => StopCondition::MetadataReceived,
            "FilesChecked" => StopCondition::FilesChecked,
            _ => StopCondition::None,
        }),
        skip_checking: p.bool_or("skip_checking", false),
        add_to_queue_top: p.bool_opt("addToTopOfQueue"),
        content_layout: Some(p.str("contentLayout"))
            .filter(|s| !s.is_empty())
            .map(|s| match s {
                "Subfolder" => ContentLayout::Subfolder,
                "NoSubfolder" => ContentLayout::NoSubfolder,
                _ => ContentLayout::Original,
            })
            .or_else(|| p.bool_opt("root_folder").map(|r| if r { ContentLayout::Subfolder } else { ContentLayout::NoSubfolder })),
        sequential: p.bool_or("sequentialDownload", false),
        first_last_piece_priority: p.bool_or("firstLastPiecePrio", false),
        forced: p.bool_or("forced", false),
        download_limit: Some(int_or("dlLimit", -1)),
        upload_limit: Some(int_or("upLimit", -1)),
        ratio_limit: Some(p.f64_opt("ratioLimit").unwrap_or(-2.0)),
        seeding_time_limit: Some(int_or("seedingTimeLimit", -2)),
        inactive_seeding_time_limit: Some(int_or("inactiveSeedingTimeLimit", -2)),
        share_limit_action: Some(share_limit_action(p.str("shareLimitAction"))),
        ..Default::default()
    };

    let mut added = Vec::new();
    let mut pending = 0u32;
    let mut failure = 0u32;
    for url in urls {
        let parsed = parse_info_hash(&url);
        let (id, cached) = {
            let d = c.session.data.lock().await;
            let id = d.source_id(&url).filter(|s| !s.is_empty()).or_else(|| parsed.as_ref().map(|x| x.2.clone()));
            let cached = id.as_ref().and_then(|i| d.metadata_for(i).cloned().flatten());
            (id, cached)
        };
        let mut params = base.clone();
        if let Some(info) = cached {
            if !file_prios.is_empty() {
                if file_prios.len() != info.files.len() {
                    return Err(ApiError::bad_params("Length of filePriorities must equal number of files in torrent"));
                }
                params.file_priorities = file_prios.clone();
            }
            let src = match c.app.save_fetched_metadata(info.hash.clone()) {
                Ok(data) => TorrentSource::Bytes { data },
                Err(_) => TorrentSource::Url { url: url.clone() },
            };
            // Metadata fetched from a URL or the swarm: not a local source.
            match c.app.add_remote_torrent(src, params).await {
                Ok(h) => added.push(h),
                Err(_) => failure += 1,
            }
        } else {
            if !file_prios.is_empty() {
                return Err(ApiError::bad_params(
                    "`filePriorities` may only be specified when metadata has already been fetched",
                ));
            }
            if !downloader.is_empty() && parsed.is_none() {
                let (app, web, plugin, u) = (c.app.clone(), c.web.clone(), downloader.clone(), url.clone());
                c.web.runtime().spawn(async move {
                    let r = tokio::time::timeout(BACKGROUND_TIMEOUT, app.search_download_torrent(u.clone(), plugin, params)).await;
                    if let Ok(Err(e)) = r {
                        web.log_warning(format!("Failed to add torrent from \"{u}\": {}", super::error_message(&e)));
                    }
                });
                pending += 1;
            } else if parsed.is_none() && is_remote_url(&url) {
                // Downloads happen in the background (202 + pending_count).
                let (app, web, u) = (c.app.clone(), c.web.clone(), url.clone());
                c.web.runtime().spawn(async move {
                    let r = tokio::time::timeout(BACKGROUND_TIMEOUT, app.add_torrent(TorrentSource::Url { url: u.clone() }, params)).await;
                    if let Ok(Err(e)) = r {
                        web.log_warning(format!("Failed to add torrent from \"{u}\": {}", super::error_message(&e)));
                    }
                });
                pending += 1;
            } else {
                match c.app.add_torrent(TorrentSource::Url { url: url.clone() }, params).await {
                    Ok(h) => added.push(h),
                    Err(_) => failure += 1,
                }
            }
        }
        let mut d = c.session.data.lock().await;
        d.remove_source(&url);
        if let Some(id) = id {
            d.remove_metadata(&id);
        }
    }

    let files = std::mem::take(&mut c.files);
    for (name, data) in files {
        if file_too_large(c, data.len()) {
            return Err(ApiError::bad_data(format!("Error: '{name}' is not a valid torrent file.")));
        }
        match c.app.add_torrent(TorrentSource::Bytes { data }, base.clone()).await {
            Ok(h) => added.push(h),
            Err(QbtError::InvalidArgument { .. }) => {
                return Err(ApiError::bad_data(format!("Error: '{name}' is not a valid torrent file.")));
            }
            Err(_) => failure += 1,
        }
    }

    if added.is_empty() && pending == 0 {
        return Err(ApiError::conflict(""));
    }
    let body = json!({
        "success_count": added.len(),
        "failure_count": failure,
        "pending_count": pending,
        "added_torrent_ids": added,
    });
    Ok(if pending > 0 { Reply::Async(body) } else { Reply::Json(body) })
}

/// qBittorrent's `parseTrackerEntries`: one URL per line, a blank line
/// starts a new tier.
pub fn parse_tracker_entries(text: &str) -> Vec<TrackerTierEntry> {
    let mut tier = 0i64;
    let mut out = Vec::new();
    for line in text.split('\n') {
        let t = line.trim();
        if t.is_empty() {
            tier = tier.saturating_add(1);
            continue;
        }
        out.push(TrackerTierEntry { url: t.to_owned(), tier });
    }
    out
}

fn valid_tracker_url(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

async fn edit_tracker(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash", "url"])?;
    let h = hash(c, "hash");
    let current: Vec<TrackerEntry> = c.app.trackers(h.clone())?.into_iter().filter(|t| t.tier >= 0).collect();
    let orig = c.params.str("url").to_owned();
    let new_url = c.params.get("newUrl").map(str::to_owned);
    let new_tier = c.params.get("tier").map(str::to_owned);
    if new_url.is_none() && new_tier.is_none() {
        return Err(ApiError::bad_params("Must specify at least one of [newUrl, tier]"));
    }
    let tier = match new_tier {
        None => None,
        Some(t) => {
            let n = parse_int32(&t).ok_or_else(|| ApiError::bad_params("tier must be an integer"))?;
            if !(0..=255).contains(&n) {
                return Err(ApiError::bad_params("tier must be between 0 and 255"));
            }
            Some(n)
        }
    };
    let new_url = match new_url {
        None => None,
        Some(u) => {
            if !valid_tracker_url(&u) {
                return Err(ApiError::bad_params("New tracker URL is invalid"));
            }
            (u != orig).then_some(u)
        }
    };
    if new_url.is_none() && tier.is_none() {
        return Ok(Reply::NoContent);
    }
    let mut entries = Vec::with_capacity(current.len());
    let mut matched = false;
    for t in &current {
        if new_url.as_deref() == Some(t.url.as_str()) {
            return Err(ApiError::conflict("New tracker URL already exists"));
        }
        let mut e = TrackerTierEntry { url: t.url.clone(), tier: t.tier };
        if t.url == orig {
            let tier_changed = tier.is_some_and(|x| x != t.tier);
            if new_url.is_none() && !tier_changed {
                return Ok(Reply::NoContent);
            }
            matched = true;
            if let Some(u) = &new_url {
                e.url = u.clone();
            }
            if let Some(x) = tier {
                e.tier = x;
            }
        }
        entries.push(e);
    }
    if !matched {
        return Err(ApiError::conflict("Tracker not found"));
    }
    c.app.replace_trackers(h.clone(), entries)?;
    skip_not_found(c.app.reannounce_torrents(vec![h]).await)?;
    Ok(Reply::NoContent)
}

fn add_peers(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hashes", "peers"])?;
    let raw: Vec<&str> = c.params.str("peers").split('|').collect();
    let peers: Vec<String> = raw.iter().filter_map(|p| super::app::parse_peer_address(p)).map(|a| a.to_string()).collect();
    if peers.is_empty() {
        return Err(ApiError::bad_params("No valid peers were specified"));
    }
    let torrents = selected(c, "hashes");
    let ids: Vec<String> = torrents.iter().map(|t| t.hash.clone()).collect();
    if !ids.is_empty() {
        skip_not_found(c.app.add_peers(ids.clone(), peers.clone()))?;
    }
    let mut m = Map::new();
    for id in ids {
        m.insert(id, json!({"added": peers.len(), "failed": raw.len() - peers.len()}));
    }
    Ok(Reply::Json(Value::Object(m)))
}

fn validate_web_seed(s: &str) -> Result<String, ApiError> {
    let decoded = percent_decode_str(s);
    let url = url::Url::parse(&decoded).map_err(|_| ApiError::bad_params(format!("\"{decoded}\" is not a valid URL")))?;
    if !matches!(url.scheme(), "http" | "https" | "ftp") {
        return Err(ApiError::bad_params("URL scheme must be one of [http, https, ftp]"));
    }
    Ok(decoded)
}

fn file_prio(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hash", "id", "priority"])?;
    let prio = parse_file_priority(c.params.str("priority"))?;
    let h = hash(c, "hash");
    let t = c.app.torrent(h.clone())?;
    if !t.has_metadata {
        return Err(ApiError::conflict("Torrent's metadata has not yet downloaded"));
    }
    let count = c.app.files(h.clone(), Vec::new())?.len() as i64;
    let mut ids = Vec::new();
    for s in c.params.str("id").split('|') {
        let id = parse_int32(s).ok_or_else(|| ApiError::bad_params("File IDs must be integers"))?;
        if id < 0 || id >= count {
            return Err(ApiError::conflict("File ID is not valid"));
        }
        ids.push(id);
    }
    c.app.set_file_priority(h, ids, prio)?;
    Ok(Reply::ok())
}

fn is_writable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c` is a valid NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

async fn set_location(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["hashes", "location"])?;
    let location = c.params.str("location").trim().to_owned();
    if location.is_empty() {
        return Err(ApiError::bad_params("Save path cannot be empty"));
    }
    let loc = location.clone();
    let made = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&loc).is_ok()).await.unwrap_or(false);
    if !made {
        return Err(ApiError::conflict("Cannot make save path"));
    }
    let torrents = selected(c, "hashes");
    for t in &torrents {
        c.web.log_info(format!(
            "WebUI Set location: moving \"{}\", from \"{}\" to \"{}\"",
            sanitize_for_log(&t.name),
            sanitize_for_log(&t.save_path),
            sanitize_for_log(&location)
        ));
    }
    let ids: Vec<String> = torrents.into_iter().map(|t| t.hash).collect();
    if !ids.is_empty() {
        skip_not_found(c.app.set_location(ids, location).await)?;
    }
    Ok(Reply::ok())
}

async fn set_path(c: &mut Call<'_>, save_path: bool) -> ApiResult {
    c.params.require(&["id", "path"])?;
    let path = c.params.str("path").to_owned();
    if save_path && path.is_empty() {
        return Err(ApiError::bad_params("Save path cannot be empty"));
    }
    if !path.is_empty() {
        let p = path.clone();
        let (made, writable) = tokio::task::spawn_blocking(move || {
            let made = std::fs::create_dir_all(&p).is_ok();
            (made, made && is_writable(std::path::Path::new(&p)))
        })
        .await
        .unwrap_or((false, false));
        if !made {
            return Err(ApiError::conflict("Cannot create target directory"));
        }
        if !writable {
            return Err(ApiError::forbidden("Cannot write to directory"));
        }
    }
    let ids = c.torrent_ids("id");
    let r = if save_path { c.app.set_save_path(ids, path).await } else { c.app.set_download_path(ids, path).await };
    skip_not_found(r)?;
    Ok(Reply::ok())
}

/// `Session::isValidCategoryName`: no `\`, no leading/trailing `/`, no `//`.
pub fn is_valid_category_name(name: &str) -> bool {
    name.is_empty() || (!name.contains('\\') && !name.starts_with('/') && !name.ends_with('/') && !name.contains("//"))
}

fn edit_category(c: &mut Call<'_>, create: bool) -> ApiResult {
    c.params.require(if create { &["category"] } else { &["category", "savePath"] })?;
    let name = c.params.str("category").to_owned();
    if name.is_empty() {
        return Err(ApiError::bad_params("Category cannot be empty"));
    }
    if create && !is_valid_category_name(&name) {
        return Err(ApiError::conflict("Incorrect category name"));
    }
    let download_path = match c.params.bool_opt("downloadPathEnabled") {
        None => CategoryDownloadPath::Default,
        Some(false) => CategoryDownloadPath::Disabled,
        Some(true) => CategoryDownloadPath::Path { path: c.params.str("downloadPath").to_owned() },
    };
    let cat = Category { name, save_path: c.params.str("savePath").to_owned(), download_path, ..Default::default() };
    if create {
        c.app.create_category(cat).map_err(|e| match e {
            QbtError::Unsupported { .. } => e.into(),
            _ => ApiError::conflict("Unable to create category"),
        })?;
    } else {
        c.app.edit_category(cat).map_err(|e| match e {
            QbtError::NotFound { .. } => ApiError::not_found_msg("Category does not exist"),
            other => other.into(),
        })?;
    }
    Ok(Reply::ok())
}

/// `Utils::Fs::isValidName` (non-Windows rules).
pub fn is_valid_file_name(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == "..")
        && name.len() <= 255
        && !name.chars().any(|c| (c as u32) < 32 || c as u32 == 127 || c == '/')
}

async fn rename_path(c: &mut Call<'_>, file: bool) -> ApiResult {
    c.params.require(&["hash", "oldPath", "newPath"])?;
    let new_path = c.params.str("newPath").to_owned();
    let leaf = new_path.rsplit('/').next().unwrap_or("");
    if !is_valid_file_name(leaf) {
        return Err(ApiError::conflict(if file {
            "File name has invalid characters"
        } else {
            "Folder name has invalid characters"
        }));
    }
    let h = hash(c, "hash");
    let old = c.params.str("oldPath").to_owned();
    let r = if file { c.app.rename_file(h, old, new_path).await } else { c.app.rename_folder(h, old, new_path).await };
    r.map_err(|e| match e {
        QbtError::NotFound { .. } => ApiError::not_found(),
        QbtError::Unsupported { .. } => e.into(),
        other => ApiError::conflict(super::error_message(&other)),
    })?;
    Ok(Reply::ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(vals: &[(i64, &str)]) -> Vec<Map<String, Value>> {
        vals.iter()
            .map(|(n, s)| json!({"n": n, "s": s, "b": n % 2 == 0, "f": *n as f64 / 3.0}).as_object().unwrap().clone())
            .collect()
    }

    fn ns(v: &[Map<String, Value>]) -> Vec<i64> {
        v.iter().map(|m| m["n"].as_i64().unwrap()).collect()
    }

    #[test]
    fn sorting_and_paging() {
        let base = items(&[(3, "c"), (1, "a"), (2, "B"), (5, "e"), (4, "d")]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 0, 0).unwrap()), [1, 2, 3, 4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", true, 0, 0).unwrap()), [5, 4, 3, 2, 1]);
        // Case-sensitive code-point order: "B" < "a".
        assert_eq!(ns(&sort_and_page(base.clone(), "s", false, 0, 0).unwrap()), [2, 1, 3, 4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "f", false, 2, 1).unwrap()), [2, 3]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 0, -2).unwrap()), [4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 10, 3).unwrap()), [4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 2, 10).unwrap()), Vec::<i64>::new());
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, -5, -100).unwrap()), [1, 2, 3, 4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 2, -100).unwrap()), Vec::<i64>::new());
        assert_eq!(ns(&sort_and_page(base.clone(), "n", false, 100, -6).unwrap()), [1, 2, 3, 4, 5]);
        assert_eq!(ns(&sort_and_page(base.clone(), "b", false, 0, 0).unwrap())[..2], [3, 1]);
        assert_eq!(sort_and_page(base.clone(), "nope", false, 0, 0).unwrap_err().message, "'sort' parameter is invalid");
        assert!(sort_and_page(Vec::new(), "nope", false, 0, 0).unwrap().is_empty());
    }

    #[test]
    fn tracker_entries() {
        let e = parse_tracker_entries("udp://a\n http://b \n\n\nhttp://c\n");
        assert_eq!(e.iter().map(|t| (t.url.as_str(), t.tier)).collect::<Vec<_>>(), [("udp://a", 0), ("http://b", 0), ("http://c", 2)]);
    }

    #[test]
    fn names() {
        assert!(is_valid_category_name(""));
        assert!(is_valid_category_name("a/b"));
        for bad in ["/a", "a/", "a//b", "a\\b"] {
            assert!(!is_valid_category_name(bad), "{bad}");
        }
        assert!(is_valid_file_name("ok.mkv"));
        for bad in ["", ".", "..", "a/b", "a\u{1}b", &"x".repeat(256)] {
            assert!(!is_valid_file_name(bad));
        }
    }

    #[test]
    fn web_seed_validation() {
        assert_eq!(validate_web_seed("http%3A%2F%2Fexample.com%2Fa").unwrap(), "http://example.com/a");
        assert_eq!(validate_web_seed("ftp://x/y").unwrap(), "ftp://x/y");
        assert_eq!(validate_web_seed("file:///etc/passwd").unwrap_err().message, "URL scheme must be one of [http, https, ftp]");
        assert_eq!(validate_web_seed("not a url").unwrap_err().message, "\"not a url\" is not a valid URL");
    }

    #[test]
    fn misc_helpers() {
        assert_eq!(safe_file_stem("abc\"\r\n;def"), "abcdef");
        assert_eq!(safe_file_stem("\"\""), "torrent");
        assert!(is_remote_url("HTTPS://x"));
        assert!(!is_remote_url("magnet:?x"));
        assert!(!is_remote_url("/tmp/x.torrent"));
        assert_eq!(parse_file_priority("6").unwrap(), FilePriority::High);
        assert_eq!(parse_file_priority("3").unwrap_err().message, "Priority is not valid");
        assert_eq!(parse_file_priority("x").unwrap_err().message, "Priority must be an integer");
        assert_eq!(qt_mid(5, 1, 2), 1..3);
        assert_eq!(qt_mid(5, 6, 2), 0..0);
    }

    #[test]
    fn torrent_object_shape() {
        let t = TorrentInfo { hash: "h".into(), dl_limit: -1, tags: vec!["a".into(), "b".into()], ..Default::default() };
        let m = torrent_json(&t);
        assert_eq!(m["hash"], "h");
        assert_eq!(m["dl_limit"], 0);
        assert_eq!(m["tags"], "a, b");
        assert_eq!(m["private"], Value::Null);
        for k in MAINDATA_ONLY {
            assert!(!m.contains_key(k));
        }
    }
}
