//! `app/*`, `clientdata/*`, `log/*`, `transfer/*`.

use std::net::SocketAddr;
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, Call, Reply, server_state_json};
use crate::model::{Cookie, LogLevel, QBT_COMPAT_VERSION, WEBAPI_VERSION};
use crate::webui::password;

/// Maximum number of entries `app/getDirectoryContent` returns.
const MAX_DIRECTORY_ENTRIES: usize = 50_000;

pub async fn app(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        // qBittorrent version this WebAPI is compatible with (clients use it
        // for feature detection), not the Neo release number.
        "version" => Ok(Reply::Text(format!("v{QBT_COMPAT_VERSION}"))),
        "webapiVersion" => Ok(Reply::Text(WEBAPI_VERSION.to_owned())),
        "buildInfo" => {
            let b = c.app.build_info();
            let platform = match b.platform.as_str() {
                "macos" | "linux" | "windows" => b.platform.clone(),
                _ => "unknown".to_owned(),
            };
            Ok(Reply::Json(json!({
                "qt": "N/A",
                "libtorrent": b.engine,
                "boost": "N/A",
                "openssl": "rustls 0.23 (aws-lc-rs)",
                "zlib": "miniz_oxide",
                "bitness": b.bitness,
                "platform": platform,
            })))
        }
        "processInfo" => Ok(Reply::Json(json!({"launch_time": c.web.launch_time()}))),
        "shutdown" => {
            let app = c.app.clone();
            c.web.runtime().spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                app.request_shutdown();
            });
            Ok(Reply::ok())
        }
        "preferences" => Ok(Reply::Json(c.web.preferences_json())),
        "setPreferences" => set_preferences(c).await,
        "defaultSavePath" => Ok(Reply::Text(c.app.default_save_path())),
        "sendTestEmail" => {
            let app = c.app.clone();
            let web = c.web.clone();
            c.web.runtime().spawn(async move {
                if let Err(e) = app.send_test_email().await {
                    web.log_warning(format!("Failed to send test e-mail: {}", super::error_message(&e)));
                }
            });
            Ok(Reply::ok())
        }
        "getDirectoryContent" => directory_content(c).await,
        "cookies" => {
            let list: Vec<Value> = c
                .app
                .cookies()
                .into_iter()
                .map(|k| {
                    json!({"name": k.name, "domain": k.domain, "path": k.path, "value": k.value, "expirationDate": k.expiration_date})
                })
                .collect();
            Ok(Reply::Json(Value::Array(list)))
        }
        "setCookies" => set_cookies(c),
        "rotateAPIKey" => {
            let key = password::generate_api_key();
            let k = key.clone();
            c.web.update_preferences(move |p| p.webui.web_ui_api_key = k).await?;
            Ok(Reply::Json(json!({"apiKey": key})))
        }
        "deleteAPIKey" => {
            c.web.update_preferences(|p| p.webui.web_ui_api_key.clear()).await?;
            Ok(Reply::NoContent)
        }
        "networkInterfaceList" => {
            let list: Vec<Value> = c
                .app
                .network_interfaces()
                .into_iter()
                .filter(|i| !i.addresses.is_empty())
                .map(|i| json!({"name": i.name, "value": i.value}))
                .collect();
            Ok(Reply::Json(Value::Array(list)))
        }
        "getFreeSpaceAtPath" => {
            c.params.require(&["path"])?;
            let path = c.params.str("path").to_owned();
            if path.is_empty() || path.contains('\0') || !std::path::Path::new(&path).is_absolute() {
                return Err(ApiError::bad_params("Invalid path"));
            }
            let app = c.app.clone();
            let free = tokio::task::spawn_blocking(move || app.free_disk_space(path)).await.unwrap_or(-1);
            if free < 0 {
                return Err(ApiError::not_found_msg("Path does not exist"));
            }
            Ok(Reply::Text(free.to_string()))
        }
        "networkInterfaceAddressList" => {
            c.params.require(&["iface"])?;
            let addrs = c.app.network_interface_addresses(c.params.str("iface").to_owned());
            Ok(Reply::Json(json!(addrs)))
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

async fn set_preferences(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["json"])?;
    // Invalid JSON or a non-object is an empty update (still 200).
    let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(c.params.str("json")) else {
        return Ok(Reply::ok());
    };
    let json = Value::Object(obj).to_string();
    let app = c.app.clone();
    // Password hashing (PBKDF2, 100k iterations) runs off the async workers.
    let r = tokio::task::spawn_blocking(move || app.set_preferences_json(json))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    r.map_err(|e| match e {
        crate::QbtError::InvalidArgument { message } => ApiError::bad_params(message),
        other => ApiError::from(other),
    })?;
    c.web.reload_config();
    Ok(Reply::ok())
}

fn set_cookies(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["cookies"])?;
    let v: Value =
        serde_json::from_str(c.params.str("cookies").trim()).map_err(|e| ApiError::bad_params(e.to_string()))?;
    let Value::Array(items) = v else {
        return Err(ApiError::bad_params("cookies must be array"));
    };
    let mut cookies = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(o) = item else { return Err(ApiError::bad_request()) };
        let s = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        cookies.push(Cookie {
            name: s("name"),
            domain: s("domain"),
            path: s("path"),
            value: s("value"),
            expiration_date: o.get("expirationDate").and_then(Value::as_i64).unwrap_or(0),
        });
    }
    c.app.set_cookies(cookies)?;
    Ok(Reply::ok())
}

fn secs(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(-1)
}

async fn directory_content(c: &mut Call<'_>) -> ApiResult {
    c.params.require(&["dirPath"])?;
    let dir = c.params.str("dirPath").to_owned();
    if dir.is_empty() || dir.starts_with(':') || dir.contains('\0') || !std::path::Path::new(&dir).is_absolute() {
        return Err(ApiError::bad_params("Invalid directory path"));
    }
    let mode = c.params.get("mode").unwrap_or("all").to_owned();
    let (want_dirs, want_files) = match mode.as_str() {
        "all" => (true, true),
        "dirs" => (true, false),
        "files" => (false, true),
        _ => return Err(ApiError::bad_params("Invalid mode, allowed values: all, dirs, files")),
    };
    let with_meta = c.params.bool_or("withMetadata", false);
    tokio::task::spawn_blocking(move || {
        let path = std::path::Path::new(&dir);
        if !path.is_dir() {
            return Err(ApiError::not_found_msg("Directory does not exist"));
        }
        let rd = std::fs::read_dir(path).map_err(|_| ApiError::not_found_msg("Directory does not exist"))?;
        let mut entries: Vec<(String, std::fs::Metadata)> = Vec::new();
        for e in rd.flatten() {
            if entries.len() >= MAX_DIRECTORY_ENTRIES {
                break;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            // Like QDir without QDir::Hidden: dot files are not listed.
            if name.starts_with('.') {
                continue;
            }
            // Follow symlinks; broken links are skipped.
            let Ok(meta) = std::fs::metadata(e.path()) else { continue };
            if (meta.is_dir() && want_dirs) || (meta.is_file() && want_files) {
                entries.push((name, meta));
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let base = dir.trim_end_matches('/');
        let list: Vec<Value> = entries
            .into_iter()
            .map(|(name, meta)| {
                if with_meta {
                    let mut o = Map::new();
                    o.insert("name".into(), json!(name));
                    o.insert("creation_date".into(), json!(secs(meta.created())));
                    o.insert("last_access_date".into(), json!(secs(meta.accessed())));
                    o.insert("last_modification_date".into(), json!(secs(meta.modified())));
                    if meta.is_dir() {
                        o.insert("type".into(), json!("dir"));
                    } else {
                        o.insert("type".into(), json!("file"));
                        o.insert("size".into(), json!(meta.len()));
                    }
                    Value::Object(o)
                } else {
                    json!(format!("{base}/{name}"))
                }
            })
            .collect();
        Ok(Reply::Json(Value::Array(list)))
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
}

pub fn clientdata(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "load" => {
            let keys = c.params.str("keys");
            if keys.is_empty() {
                return Ok(Reply::Json(Value::Object(c.web.clientdata().load_all())));
            }
            let v: Value = serde_json::from_str(keys).map_err(|e| ApiError::bad_params(e.to_string()))?;
            let Value::Array(items) = v else { return Err(ApiError::bad_params("`keys` must be an array")) };
            let mut list = Vec::with_capacity(items.len());
            for i in items {
                let Value::String(s) = i else { return Err(ApiError::bad_params("Items of `keys` must be strings")) };
                list.push(s);
            }
            Ok(Reply::Json(Value::Object(c.web.clientdata().load_keys(&list))))
        }
        "store" => {
            c.params.require(&["data"])?;
            let v: Value = serde_json::from_str(c.params.str("data")).map_err(|e| ApiError::bad_params(e.to_string()))?;
            let Value::Object(obj) = v else { return Err(ApiError::bad_params("`data` must be an object")) };
            c.web.clientdata().store(obj).map_err(ApiError::conflict)?;
            Ok(Reply::NoContent)
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

fn last_known_id(c: &Call<'_>) -> i64 {
    c.params.int_opt("last_known_id").unwrap_or(-1)
}

pub fn log(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "main" => {
            let mut levels = Vec::new();
            for (key, level) in [
                ("normal", LogLevel::Normal),
                ("info", LogLevel::Info),
                ("warning", LogLevel::Warning),
                ("critical", LogLevel::Critical),
            ] {
                if c.params.bool_or(key, true) {
                    levels.push(level);
                }
            }
            let msgs = if levels.is_empty() { Vec::new() } else { c.app.main_log(last_known_id(c), levels) };
            let list: Vec<Value> = msgs
                .into_iter()
                .map(|m| {
                    json!({"id": m.id, "message": m.message, "timestamp": m.timestamp.div_euclid(1000), "type": m.level.as_flag()})
                })
                .collect();
            Ok(Reply::Json(Value::Array(list)))
        }
        "peers" => {
            let list: Vec<Value> = c
                .app
                .peer_log(last_known_id(c))
                .into_iter()
                .map(|p| {
                    json!({"id": p.id, "ip": p.ip, "timestamp": p.timestamp.div_euclid(1000), "blocked": p.blocked, "reason": p.reason})
                })
                .collect();
            Ok(Reply::Json(Value::Array(list)))
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

/// `host:port` / `[v6]:port` with a non-zero port (qBittorrent `PeerAddress::parse`).
pub fn parse_peer_address(s: &str) -> Option<SocketAddr> {
    let sa: SocketAddr = s.trim().parse().ok()?;
    (sa.port() != 0).then(|| SocketAddr::new(sa.ip().to_canonical(), sa.port()))
}

pub fn transfer(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "info" => {
            let full = server_state_json(&c.app.server_state(), &c.app.preferences());
            let mut o = Map::new();
            for k in [
                "dl_info_speed",
                "dl_info_data",
                "up_info_speed",
                "up_info_data",
                "dl_rate_limit",
                "up_rate_limit",
                "dht_nodes",
                "connection_status",
                "last_external_address_v4",
                "last_external_address_v6",
            ] {
                if let Some(v) = full.get(k) {
                    o.insert(k.into(), v.clone());
                }
            }
            Ok(Reply::Json(Value::Object(o)))
        }
        "speedLimitsMode" => Ok(Reply::Text(if c.app.alt_speed_limits_enabled() { "1" } else { "0" }.into())),
        "setSpeedLimitsMode" => {
            c.params.require(&["mode"])?;
            let mode = c.params.int_opt("mode").ok_or_else(|| ApiError::bad_params("'mode': invalid argument"))?;
            c.app.set_alt_speed_limits_enabled(mode != 0);
            Ok(Reply::ok())
        }
        "toggleSpeedLimitsMode" => {
            c.app.set_alt_speed_limits_enabled(!c.app.alt_speed_limits_enabled());
            Ok(Reply::ok())
        }
        "uploadLimit" => Ok(Reply::Text(c.app.global_upload_limit().max(0).to_string())),
        "downloadLimit" => Ok(Reply::Text(c.app.global_download_limit().max(0).to_string())),
        "setUploadLimit" | "setDownloadLimit" => {
            c.params.require(&["limit"])?;
            // 0 / negative = unlimited (stored as 0).
            let limit = c.params.i64("limit").max(0);
            if action == "setUploadLimit" {
                c.app.set_global_upload_limit(limit)?;
            } else {
                c.app.set_global_download_limit(limit)?;
            }
            Ok(Reply::ok())
        }
        "banPeers" => {
            c.params.require(&["peers"])?;
            let ips: Vec<String> =
                c.params.str("peers").split('|').filter_map(parse_peer_address).map(|a| a.ip().to_string()).collect();
            if !ips.is_empty() {
                c.app.ban_peers(ips)?;
            }
            Ok(Reply::ok())
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_addresses() {
        assert_eq!(parse_peer_address("1.2.3.4:5").unwrap().to_string(), "1.2.3.4:5");
        assert_eq!(parse_peer_address(" [::1]:6881 ").unwrap().to_string(), "[::1]:6881");
        assert_eq!(parse_peer_address("[::ffff:1.2.3.4]:9").unwrap().to_string(), "1.2.3.4:9");
        for bad in ["1.2.3.4", "1.2.3.4:0", "::1:5", "host:80", "", "1.2.3.4:70000", "[::1]"] {
            assert!(parse_peer_address(bad).is_none(), "{bad}");
        }
    }
}
