//! `search/*`. Search jobs are tracked per WebUI session like qBittorrent
//! (a session only sees and controls the jobs it started).

use serde_json::{Value, json};

use super::{ApiError, ApiResult, Call, MAX_SESSION_ITEMS, Reply};
use crate::QbtError;
use crate::model::*;

/// qBittorrent's `MAX_CONCURRENT_SEARCHES` (per session).
const MAX_CONCURRENT_SEARCHES: usize = 5;

fn category_name(id: &str) -> String {
    match id {
        "all" => "All categories",
        "anime" => "Anime",
        "books" => "Books",
        "games" => "Games",
        "movies" => "Movies",
        "music" => "Music",
        "pictures" => "Pictures",
        "software" => "Software",
        "tv" => "TV shows",
        other => other,
    }
    .to_owned()
}

fn plugin_json(p: &SearchPlugin) -> Value {
    let mut cats: Vec<&SearchCategory> = p.supported_categories.iter().filter(|c| c.id != "all").collect();
    cats.sort_by_key(|c| c.id.to_lowercase());
    let mut list = vec![json!({"id": "all", "name": category_name("all")})];
    list.extend(cats.into_iter().map(|c| {
        let name = if c.name.is_empty() { category_name(&c.id) } else { c.name.clone() };
        json!({"id": c.id, "name": name})
    }));
    json!({
        "name": p.name, "version": p.version, "fullName": p.full_name, "url": p.url,
        "supportedCategories": list, "enabled": p.enabled,
    })
}

async fn owned_id(c: &Call<'_>) -> Result<i64, ApiError> {
    c.params.require(&["id"])?;
    let id = c.params.int("id");
    if c.session.data.lock().await.searches.contains(&id) { Ok(id) } else { Err(ApiError::not_found()) }
}

fn status_str(s: SearchJobStatus) -> &'static str {
    match s {
        SearchJobStatus::Running => "Running",
        SearchJobStatus::Stopped => "Stopped",
    }
}

pub async fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "start" => {
            c.params.require(&["pattern", "category", "plugins"])?;
            if !c.app.search_engine_status().python_found {
                return Err(ApiError::conflict("Python must be installed to use the Search Engine."));
            }
            let pattern = c.params.str("pattern").trim().to_owned();
            let category = c.params.str("category").trim().to_owned();
            let requested: Vec<String> = c.params.str("plugins").split('|').map(str::to_owned).collect();
            let plugins = if requested.len() == 1 {
                match requested[0].to_lowercase().as_str() {
                    "all" => c.app.search_plugins().into_iter().map(|p| p.name).collect(),
                    "enabled" | "multi" => {
                        c.app.search_plugins().into_iter().filter(|p| p.enabled).map(|p| p.name).collect()
                    }
                    _ => requested,
                }
            } else {
                requested
            };
            let jobs: Vec<i64> = c.session.data.lock().await.searches.iter().copied().collect();
            let running = jobs
                .iter()
                .filter(|id| {
                    c.app.search_status(Some(**id)).ok().and_then(|v| v.into_iter().next()).is_some_and(|j| j.status == SearchJobStatus::Running)
                })
                .count();
            if running >= MAX_CONCURRENT_SEARCHES {
                return Err(ApiError::conflict(format!(
                    "Unable to create more than {MAX_CONCURRENT_SEARCHES} concurrent searches."
                )));
            }
            let id = c.app.search_start(pattern, plugins, category)?;
            let mut d = c.session.data.lock().await;
            d.searches.push_back(id);
            if d.searches.len() > MAX_SESSION_ITEMS
                && let Some(old) = d.searches.pop_front()
            {
                let _ = c.app.search_delete(old);
            }
            Ok(Reply::Json(json!({"id": id})))
        }
        "stop" => {
            let id = owned_id(c).await?;
            c.app.search_stop(id)?;
            Ok(Reply::ok())
        }
        "status" => {
            let id = c.params.int("id");
            let ids: Vec<i64> = {
                let d = c.session.data.lock().await;
                if id != 0 {
                    if !d.searches.contains(&id) {
                        return Err(ApiError::not_found());
                    }
                    vec![id]
                } else {
                    d.searches.iter().copied().collect()
                }
            };
            let mut list = Vec::new();
            for id in ids {
                match c.app.search_status(Some(id)) {
                    Ok(v) => list.extend(
                        v.into_iter().map(|j| json!({"id": j.id, "status": status_str(j.status), "total": j.total})),
                    ),
                    Err(QbtError::NotFound { .. }) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            Ok(Reply::Json(Value::Array(list)))
        }
        "results" => {
            let id = owned_id(c).await?;
            let page = c.app.search_results(id, c.params.int("limit"), c.params.int("offset")).map_err(|e| match e {
                QbtError::NotFound { .. } => ApiError::not_found(),
                QbtError::InvalidArgument { .. } | QbtError::Conflict { .. } => ApiError::conflict("Offset is out of range"),
                other => other.into(),
            })?;
            Ok(Reply::Json(json!({"status": status_str(page.status), "results": page.results, "total": page.total})))
        }
        "delete" => {
            let id = owned_id(c).await?;
            match c.app.search_delete(id) {
                Ok(()) | Err(QbtError::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
            c.session.data.lock().await.searches.retain(|x| *x != id);
            Ok(Reply::ok())
        }
        "downloadTorrent" => {
            c.params.require(&["torrentUrl", "pluginName"])?;
            let url = c.params.str("torrentUrl").to_owned();
            let plugin = c.params.str("pluginName").to_owned();
            let (app, web) = (c.app.clone(), c.web.clone());
            c.web.runtime().spawn(async move {
                let r = if url.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("magnet:")) {
                    app.add_torrent(TorrentSource::Url { url: url.clone() }, AddTorrentParams::default()).await.map(|_| ())
                } else {
                    app.search_download_torrent(url.clone(), plugin, AddTorrentParams::default()).await
                };
                if let Err(e) = r {
                    web.log_warning(format!("Failed to add search result \"{url}\": {}", super::error_message(&e)));
                }
            });
            Ok(Reply::ok())
        }
        "plugins" => Ok(Reply::Json(Value::Array(c.app.search_plugins().iter().map(plugin_json).collect()))),
        "installPlugin" => {
            c.params.require(&["sources"])?;
            let sources: Vec<String> = c.params.str("sources").split('|').map(str::to_owned).collect();
            let (app, web) = (c.app.clone(), c.web.clone());
            c.web.runtime().spawn(async move {
                if let Err(e) = app.search_install_plugins(sources).await {
                    web.log_warning(format!("Failed to install search plugin: {}", super::error_message(&e)));
                }
            });
            Ok(Reply::ok())
        }
        "uninstallPlugin" | "enablePlugin" => {
            let enable_action = action == "enablePlugin";
            c.params.require(if enable_action { &["names", "enable"] } else { &["names"] })?;
            let names: Vec<String> = c.params.str("names").split('|').map(|s| s.trim().to_owned()).collect();
            let r = if enable_action {
                c.app.search_enable_plugins(names, c.params.str("enable").trim().eq_ignore_ascii_case("true"))
            } else {
                c.app.search_uninstall_plugins(names)
            };
            match r {
                Err(e @ QbtError::Unsupported { .. }) => Err(e.into()),
                _ => Ok(Reply::ok()),
            }
        }
        "updatePlugins" => {
            let (app, web) = (c.app.clone(), c.web.clone());
            c.web.runtime().spawn(async move {
                if let Err(e) = app.search_update_plugins().await {
                    web.log_info(format!("Failed to check for plugin updates: {}", super::error_message(&e)));
                }
            });
            Ok(Reply::ok())
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_categories_start_with_all() {
        let p = SearchPlugin {
            name: "x".into(),
            supported_categories: vec![
                SearchCategory { id: "tv".into(), name: String::new() },
                SearchCategory { id: "Anime".into(), name: "Anime".into() },
            ],
            ..Default::default()
        };
        let j = plugin_json(&p);
        assert_eq!(j["supportedCategories"][0], json!({"id": "all", "name": "All categories"}));
        assert_eq!(j["supportedCategories"][1]["id"], "Anime");
        assert_eq!(j["supportedCategories"][2], json!({"id": "tv", "name": "TV shows"}));
    }
}
