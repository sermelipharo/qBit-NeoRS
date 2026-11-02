//! `search/*`. Search jobs are shared by every WebUI session and can be kept
//! across restarts (qBittorrent 5.3 `SearchJobManager`, see
//! `webui/search_jobs.rs`).

use serde_json::{Value, json};

use super::{ApiError, ApiResult, Call, Reply};
use crate::QbtError;
use crate::model::*;
use crate::webui::search_jobs::JobInfo;

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

/// qBittorrent's `jobInfoToJSON` (same key order).
fn job_json(j: &JobInfo) -> Value {
    json!({
        "id": j.id,
        "status": if j.active { "Running" } else { "Stopped" },
        "pattern": j.pattern,
        "category": j.category,
        "plugins": j.plugins,
        "total": j.total,
    })
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
            let id = c.web.search_jobs().start_search(c.app.as_ref(), &pattern, &category, &plugins)?;
            Ok(Reply::Json(json!({"id": id})))
        }
        "stop" => {
            c.params.require(&["id"])?;
            if !c.web.search_jobs().stop_search(c.app.as_ref(), c.params.int("id")) {
                return Err(ApiError::not_found());
            }
            Ok(Reply::ok())
        }
        "status" => {
            let id = c.params.int("id");
            let jobs = c.web.search_jobs();
            let list: Vec<Value> = if id != 0 {
                vec![job_json(&jobs.job_info(c.app.as_ref(), id).ok_or_else(ApiError::not_found)?)]
            } else {
                jobs.all_job_infos(c.app.as_ref()).iter().map(job_json).collect()
            };
            Ok(Reply::Json(Value::Array(list)))
        }
        "results" => {
            c.params.require(&["id"])?;
            let id = c.params.int("id");
            let page = c.web.search_jobs().results(c.app.as_ref(), id, c.params.int("limit"), c.params.int("offset")).map_err(|e| match e {
                QbtError::NotFound { .. } => ApiError::not_found(),
                QbtError::InvalidArgument { .. } | QbtError::Conflict { .. } => ApiError::conflict("Offset is out of range"),
                other => other.into(),
            })?;
            Ok(Reply::Json(json!({"status": status_str(page.status), "results": page.results, "total": page.total})))
        }
        "delete" => {
            c.params.require(&["id"])?;
            if !c.web.search_jobs().delete_search(c.app.as_ref(), c.params.int("id")) {
                return Err(ApiError::not_found());
            }
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
