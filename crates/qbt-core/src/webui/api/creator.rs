//! `torrentcreator/*` (one creator shared by all sessions).

use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, Call, Reply};
use crate::QbtError;
use crate::model::*;
use crate::webui::http::{parse_int32, percent_decode_str};

/// Task parameters the core's task record does not keep, reported by
/// `torrentcreator/status`.
#[derive(Clone, Debug, Default)]
pub struct CreatorExtras {
    pub private: bool,
    pub comment: String,
    pub source: String,
    pub trackers: Vec<String>,
    pub url_seeds: Vec<String>,
}

/// Qt's default `QDateTime::toString()` text format, local time.
fn qt_date(secs: i64) -> Option<String> {
    if secs <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%a %b %-d %H:%M:%S %Y").to_string())
}

fn format_str(f: TorrentFormat) -> &'static str {
    match f {
        TorrentFormat::V1 => "v1",
        TorrentFormat::V2 => "v2",
        TorrentFormat::Hybrid => "hybrid",
    }
}

fn status_str(s: CreatorTaskStatus) -> &'static str {
    match s {
        CreatorTaskStatus::Queued => "Queued",
        CreatorTaskStatus::Running => "Running",
        CreatorTaskStatus::Finished => "Finished",
        CreatorTaskStatus::Failed => "Failed",
    }
}

fn task_json(t: &CreatorTask, extras: Option<&CreatorExtras>) -> Value {
    let x = extras.cloned().unwrap_or_default();
    let mut m = Map::new();
    m.insert("taskID".into(), json!(t.id));
    m.insert("sourcePath".into(), json!(t.source_path));
    m.insert("pieceSize".into(), json!(t.piece_size));
    m.insert("private".into(), json!(x.private));
    m.insert("timeAdded".into(), json!(qt_date(t.time_added).unwrap_or_default()));
    m.insert("format".into(), json!(format_str(t.format)));
    m.insert("status".into(), json!(status_str(t.status)));
    if !x.comment.is_empty() {
        m.insert("comment".into(), json!(x.comment));
    }
    if !t.torrent_file_path.is_empty() {
        m.insert("torrentFilePath".into(), json!(t.torrent_file_path));
    }
    if !x.source.is_empty() {
        m.insert("source".into(), json!(x.source));
    }
    if !x.trackers.is_empty() {
        m.insert("trackers".into(), json!(x.trackers));
    }
    if !x.url_seeds.is_empty() {
        m.insert("urlSeeds".into(), json!(x.url_seeds));
    }
    if let Some(d) = qt_date(t.time_started) {
        m.insert("timeStarted".into(), json!(d));
    }
    if let Some(d) = qt_date(t.time_finished) {
        m.insert("timeFinished".into(), json!(d));
    }
    match t.status {
        CreatorTaskStatus::Failed => {
            m.insert("errorMessage".into(), json!(t.error_message));
        }
        CreatorTaskStatus::Running => {
            m.insert("progress".into(), json!(t.progress.round().clamp(0.0, 100.0) as i64));
        }
        _ => {}
    }
    Value::Object(m)
}

/// The output path of a created torrent is restricted to an absolute
/// `*.torrent` path that is not a symlink or a non-regular file, so the
/// endpoint cannot be used to overwrite arbitrary files.
pub fn validate_output_path(path: &str) -> Result<(), ApiError> {
    if path.is_empty() {
        return Ok(());
    }
    let p = std::path::Path::new(path);
    let bad = || ApiError::bad_params("Invalid torrent file path");
    if path.contains('\0') || !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(bad());
    }
    let is_torrent = p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("torrent"));
    if !is_torrent {
        return Err(bad());
    }
    match std::fs::symlink_metadata(p) {
        Ok(m) if !m.file_type().is_file() => Err(bad()),
        _ => Ok(()),
    }
}

fn split_urls(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split('|').map(percent_decode_str).collect()
}

pub fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "addTask" => {
            c.params.require(&["sourcePath"])?;
            let p = &c.params;
            let torrent_file_path = p.str("torrentFilePath").to_owned();
            validate_output_path(&torrent_file_path)?;
            let source_path = p.str("sourcePath").to_owned();
            if source_path.is_empty() || !std::path::Path::new(&source_path).is_absolute() {
                return Err(ApiError::bad_params("Invalid source path"));
            }
            let format = match p.str("format").to_lowercase().as_str() {
                "v1" => TorrentFormat::V1,
                "v2" => TorrentFormat::V2,
                _ => TorrentFormat::Hybrid,
            };
            // Empty tracker entries start a new tier; empty web seeds are dropped.
            let trackers = split_urls(p.str("trackers"));
            let url_seeds: Vec<String> = split_urls(p.str("urlSeeds")).into_iter().filter(|u| !u.is_empty()).collect();
            let params = TorrentCreatorParams {
                source_path,
                torrent_file_path: torrent_file_path.clone(),
                format,
                piece_size: parse_int32(p.str("pieceSize")).unwrap_or(0),
                optimize_alignment: p.bool_or("optimizeAlignment", true),
                padding_file_limit: parse_int32(p.str("paddedFileSizeLimit")).unwrap_or(-1),
                private: p.bool_or("private", false),
                start_seeding: p.bool_or("startSeeding", torrent_file_path.is_empty()),
                ignore_share_limits: false,
                trackers: trackers.clone(),
                url_seeds: url_seeds.clone(),
                comment: p.str("comment").to_owned(),
                source: p.str("source").to_owned(),
            };
            let extras = CreatorExtras {
                private: params.private,
                comment: params.comment.clone(),
                source: params.source.clone(),
                trackers,
                url_seeds,
            };
            let id = c.app.creator_add_task(params).map_err(|e| match e {
                QbtError::Conflict { .. } => ApiError::conflict("Too many active tasks"),
                other => other.into(),
            })?;
            c.web.set_creator_extras(&id, extras);
            Ok(Reply::Json(json!({"taskID": id})))
        }
        "status" => {
            let id = c.params.str("taskID").to_owned();
            let tasks = if id.is_empty() { c.app.creator_tasks() } else { vec![c.app.creator_task(id)?] };
            let list: Vec<Value> = tasks.iter().map(|t| task_json(t, c.web.creator_extras(&t.id).as_ref())).collect();
            Ok(Reply::Json(Value::Array(list)))
        }
        "torrentFile" => {
            c.params.require(&["taskID"])?;
            let id = c.params.str("taskID").to_owned();
            let task = c.app.creator_task(id.clone())?;
            match task.status {
                CreatorTaskStatus::Queued | CreatorTaskStatus::Running => {
                    return Err(ApiError::conflict("Torrent creation is still unfinished."));
                }
                CreatorTaskStatus::Failed => return Err(ApiError::conflict("Torrent creation failed.")),
                CreatorTaskStatus::Finished => {}
            }
            let data = c.app.creator_torrent_file(id.clone()).map_err(|e| match e {
                QbtError::NotFound { .. } => ApiError::not_found(),
                QbtError::Unsupported { .. } => e.into(),
                other => ApiError::conflict(super::error_message(&other)),
            })?;
            let stem: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(64).collect();
            Ok(Reply::Bytes { data, mime: "application/x-bittorrent", filename: Some(format!("{stem}.torrent")) })
        }
        "deleteTask" => {
            c.params.require(&["taskID"])?;
            let id = c.params.str("taskID").to_owned();
            c.app.creator_delete_task(id.clone())?;
            c.web.remove_creator_extras(&id);
            Ok(Reply::ok())
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_rules() {
        assert!(validate_output_path("").is_ok());
        assert!(validate_output_path("/tmp/x.torrent").is_ok());
        assert!(validate_output_path("/tmp/X.TORRENT").is_ok());
        for bad in ["relative.torrent", "/etc/passwd", "/tmp/../etc/x.torrent", "/tmp/x.torrent.sh", "/tmp/a\0.torrent"] {
            assert!(validate_output_path(bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("secret");
        std::fs::write(&target, "x").unwrap();
        let link = dir.path().join("link.torrent");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(validate_output_path(link.to_str().unwrap()).is_err());
        let d = dir.path().join("d.torrent");
        std::fs::create_dir(&d).unwrap();
        assert!(validate_output_path(d.to_str().unwrap()).is_err());
    }

    #[test]
    fn status_shape() {
        let t = CreatorTask {
            id: "abc".into(),
            status: CreatorTaskStatus::Running,
            progress: 41.6,
            time_added: 1_700_000_000,
            ..Default::default()
        };
        let x = CreatorExtras { comment: "c".into(), trackers: vec!["u".into(), "".into()], ..Default::default() };
        let j = task_json(&t, Some(&x));
        assert_eq!(j["progress"], 42);
        assert_eq!(j["status"], "Running");
        assert_eq!(j["comment"], "c");
        assert_eq!(j["trackers"], json!(["u", ""]));
        assert!(j.get("timeStarted").is_none());
        assert!(j["timeAdded"].as_str().unwrap().contains("2023"));
        assert!(j.get("errorMessage").is_none());
    }
}
