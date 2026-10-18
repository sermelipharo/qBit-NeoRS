//! `sync/maindata`, `sync/torrentPeers` and the `server_state` object.

use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, Call, Reply, category_json};
use crate::model::ServerState;
use crate::prefs::Preferences;
use crate::webui::sync::MaindataInput;

/// `server_state` (also the source of `transfer/info`).
pub fn server_state_json(s: &ServerState, prefs: &Preferences) -> Map<String, Value> {
    let mut m = match serde_json::to_value(s) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    // Removed from the WebAPI in 2.15.0 (subcategories are always on).
    m.shift_remove("use_subcategories");
    if s.alltime_download <= 0 || s.alltime_upload <= 0 {
        m.insert("global_ratio".into(), json!("-"));
    }
    for (key, v) in [
        ("read_cache_hits", s.read_cache_hits),
        ("write_cache_overload", s.write_cache_overload),
        ("read_cache_overload", s.read_cache_overload),
    ] {
        if v <= 0.0 {
            m.insert(key.into(), json!("0"));
        }
    }
    m.insert("queueing".into(), json!(prefs.bittorrent.queueing_enabled));
    if s.refresh_interval <= 0 {
        m.insert("refresh_interval".into(), json!(prefs.advanced.refresh_interval));
    }
    m
}

pub async fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "maindata" => {
            let rid = c.params.int("rid");
            let snap = c.app.main_data_snapshot();
            let prefs = c.app.preferences();
            let input = MaindataInput {
                torrents: snap.torrents,
                categories: snap.categories.iter().map(|cat| (cat.name.clone(), category_json(cat))).collect(),
                tags: snap.tags,
                trackers: snap.trackers,
                server_state: server_state_json(&snap.server_state, &prefs),
            };
            let mut data = c.session.data.lock().await;
            Ok(Reply::Json(data.maindata.poll(rid, input)))
        }
        "torrentPeers" => {
            let hash = c.params.str("hash").trim().to_ascii_lowercase();
            let peers = c.app.peers(hash).map_err(|e| match e {
                crate::QbtError::NotFound { .. } => ApiError::not_found(),
                other => other.into(),
            })?;
            let prefs = c.app.preferences();
            let countries = prefs.advanced.resolve_peer_countries;
            let host_names = prefs.advanced.resolve_peer_host_names;
            let mut map = Map::new();
            for p in peers {
                if p.ip.is_empty() {
                    continue;
                }
                let key = if p.ip.contains(':') { format!("[{}]:{}", p.ip, p.port) } else { format!("{}:{}", p.ip, p.port) };
                let mut o = match serde_json::to_value(&p) {
                    Ok(Value::Object(o)) => o,
                    _ => continue,
                };
                if !countries {
                    o.insert("country_code".into(), Value::Null);
                    o.insert("country".into(), Value::Null);
                } else if let Some(Value::String(cc)) = o.get_mut("country_code") {
                    *cc = cc.to_ascii_lowercase();
                }
                if !host_names {
                    o.insert("host_name".into(), json!(""));
                }
                map.insert(key, Value::Object(o));
            }
            let mut data = Map::new();
            data.insert("show_flags".into(), json!(countries));
            data.insert("peers".into(), Value::Object(map));
            let rid = c.params.int("rid");
            let mut sd = c.session.data.lock().await;
            Ok(Reply::Json(sd.peers.poll(rid, data)))
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_state_quirks() {
        let mut p = Preferences::default();
        p.bittorrent.queueing_enabled = true;
        let s = ServerState { alltime_download: 0, alltime_upload: 5, ..Default::default() };
        let m = server_state_json(&s, &p);
        assert_eq!(m["global_ratio"], "-");
        assert_eq!(m["read_cache_hits"], "0");
        assert_eq!(m["queueing"], true);
        assert_eq!(m["refresh_interval"], 1500);
        assert_eq!(m["connection_status"], "disconnected");
        assert!(m.get("use_subcategories").is_none());
        let s = ServerState { alltime_download: 100, alltime_upload: 150, global_ratio: 1.5, refresh_interval: 900, ..Default::default() };
        let m = server_state_json(&s, &p);
        assert_eq!(m["global_ratio"], "1.50");
        assert_eq!(m["refresh_interval"], 900);
    }
}
