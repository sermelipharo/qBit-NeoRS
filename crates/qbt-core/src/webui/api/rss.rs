//! `rss/*`. Item paths use `\` as the folder separator. The RSS subsystem
//! produces the exact WebAPI JSON for items and rules; session errors come
//! back as conflicts (HTTP 409 with the session's message).

use serde_json::{Map, Value, json};

use super::{ApiError, ApiResult, Call, Reply};
use crate::QbtError;

/// 409 with the RSS session's message (501 while unimplemented).
fn conflict(e: QbtError) -> ApiError {
    match e {
        QbtError::Unsupported { .. } => ApiError::from(e),
        other => ApiError::conflict(super::error_message(&other)),
    }
}

pub fn dispatch(c: &mut Call<'_>, action: &str) -> ApiResult {
    match action {
        "addFolder" => {
            c.params.require(&["path"])?;
            c.app.rss_add_folder(c.params.str("path").to_owned()).map_err(conflict)?;
            Ok(Reply::ok())
        }
        "addFeed" => {
            c.params.require(&["url", "path"])?;
            let url = c.params.str("url").to_owned();
            let path = c.params.str("path");
            let path = if path.is_empty() { url.clone() } else { path.to_owned() };
            let interval = c.params.i64("refreshInterval").max(0);
            c.app.rss_add_feed(url, path, interval).map_err(conflict)?;
            Ok(Reply::ok())
        }
        "setFeedURL" => {
            c.params.require(&["path", "url"])?;
            c.app.rss_set_feed_url(c.params.str("path").to_owned(), c.params.str("url").to_owned()).map_err(conflict)?;
            Ok(Reply::ok())
        }
        "setFeedRefreshInterval" => {
            c.params.require(&["path", "refreshInterval"])?;
            let interval = crate::webui::http::parse_int_str(c.params.str("refreshInterval"))
                .filter(|v| *v >= 0)
                .ok_or_else(|| ApiError::bad_params("Invalid 'refreshInterval' value"))?;
            c.app.rss_set_feed_refresh_interval(c.params.str("path").to_owned(), interval).map_err(|e| match e {
                QbtError::InvalidArgument { message } => ApiError::bad_params(message),
                other => conflict(other),
            })?;
            Ok(Reply::NoContent)
        }
        "removeItem" => {
            c.params.require(&["path"])?;
            c.app.rss_remove_item(c.params.str("path").to_owned()).map_err(conflict)?;
            Ok(Reply::ok())
        }
        "moveItem" => {
            c.params.require(&["itemPath", "destPath"])?;
            c.app.rss_move_item(c.params.str("itemPath").to_owned(), c.params.str("destPath").to_owned()).map_err(conflict)?;
            Ok(Reply::ok())
        }
        "items" => Ok(Reply::Json(c.app.rss_items_json(c.params.bool_or("withData", false)))),
        "markAsRead" => {
            c.params.require(&["itemPath"])?;
            let article = c.params.get("articleId").map(str::to_owned);
            match c.app.rss_mark_as_read(c.params.str("itemPath").to_owned(), article) {
                Ok(()) => Ok(Reply::ok()),
                // Unknown items are ignored silently.
                Err(QbtError::NotFound { .. }) => Ok(Reply::NoContent),
                Err(e) => Err(conflict(e)),
            }
        }
        "refreshItem" => {
            c.params.require(&["itemPath"])?;
            match c.app.rss_refresh_item(c.params.str("itemPath").to_owned()) {
                Ok(()) | Err(QbtError::NotFound { .. }) => Ok(Reply::ok()),
                Err(e) => Err(conflict(e)),
            }
        }
        "setRule" => {
            c.params.require(&["ruleName", "ruleDef"])?;
            c.app
                .rss_set_rule_json(c.params.str("ruleName").to_owned(), c.params.str("ruleDef").to_owned())
                .map_err(conflict)?;
            Ok(Reply::ok())
        }
        "renameRule" => {
            c.params.require(&["ruleName", "newRuleName"])?;
            // No error if the rule does not exist (qBittorrent behaviour).
            match c.app.rss_rename_rule(c.params.str("ruleName").to_owned(), c.params.str("newRuleName").to_owned()) {
                Err(e @ QbtError::Unsupported { .. }) => Err(e.into()),
                _ => Ok(Reply::ok()),
            }
        }
        "removeRule" => {
            c.params.require(&["ruleName"])?;
            match c.app.rss_remove_rule(c.params.str("ruleName").to_owned()) {
                Err(e @ QbtError::Unsupported { .. }) => Err(e.into()),
                _ => Ok(Reply::ok()),
            }
        }
        "rules" => Ok(Reply::Json(c.app.rss_rules_json())),
        "matchingArticles" => {
            c.params.require(&["ruleName"])?;
            let list = match c.app.rss_matching_articles(c.params.str("ruleName").to_owned()) {
                Ok(l) => l,
                Err(QbtError::NotFound { .. }) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            let mut m = Map::new();
            for f in list.into_iter().filter(|f| !f.article_titles.is_empty()) {
                m.insert(f.feed_name, json!(f.article_titles));
            }
            Ok(Reply::Json(Value::Object(m)))
        }
        _ => Err(ApiError::endpoint_not_found()),
    }
}
