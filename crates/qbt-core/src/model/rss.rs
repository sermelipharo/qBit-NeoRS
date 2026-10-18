use serde::{Deserialize, Serialize};

use super::{ContentLayout, StopCondition};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RssArticle {
    pub id: String,
    /// Unix seconds. JSON: RFC 2822 text (`03 Oct 2026 04:05:06 +0000`), as
    /// in qBittorrent's `rss/items`.
    #[serde(with = "rfc2822_seconds")]
    pub date: i64,
    pub title: String,
    pub author: String,
    pub description: String,
    #[serde(rename = "torrentURL")]
    pub torrent_url: String,
    pub link: String,
    #[serde(rename = "isRead")]
    pub is_read: bool,
}

/// A feed. Serializes like a feed object of `rss/items?withData=true`
/// (GUI-only fields are skipped).
#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RssFeed {
    /// `{uuid}` with braces, like `QUuid::toString()`.
    pub uid: String,
    pub url: String,
    /// Full item path, folders separated by `\`.
    #[serde(skip)]
    pub path: String,
    pub title: String,
    #[serde(rename = "lastBuildDate")]
    pub last_build_date: String,
    #[serde(rename = "isLoading")]
    pub is_loading: bool,
    #[serde(rename = "hasError")]
    pub has_error: bool,
    /// Per-feed refresh interval in seconds, 0 = global.
    #[serde(rename = "refreshInterval", skip_serializing_if = "is_zero")]
    pub refresh_interval: i64,
    #[serde(skip)]
    pub unread_count: i64,
    pub articles: Vec<RssArticle>,
    /// Downloaded favicon (`data/rss/articles/<uid>.ico`), empty if none.
    #[serde(skip)]
    pub icon_path: String,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Unix seconds as `Qt::RFC2822Date` text.
mod rfc2822_seconds {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(secs: &i64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&crate::rss::date::format_rfc2822(secs.saturating_mul(1000)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Number(i64),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Text(t) => crate::rss::date::parse_rfc2822(&t).map_or(0, |ms| ms.div_euclid(1000)),
            Raw::Number(n) => n,
        })
    }
}

/// A node of the RSS tree (folder or feed).
#[derive(Clone, Debug, PartialEq, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RssFolder {
    pub path: String,
    pub name: String,
    pub folders: Vec<RssFolder>,
    pub feeds: Vec<RssFeed>,
}

/// Stored AddTorrentParams subset (qBittorrent JSON names) used by RSS
/// auto-download rules (`torrentParams`) and watched folders.
/// `Default` is every field at "Default": limits -1 (global) and -2 (global
/// share limits), as when the JSON omits them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct TorrentParamsTemplate {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub save_path: String,
    #[serde(default)]
    pub use_download_path: Option<bool>,
    #[serde(default)]
    pub download_path: String,
    #[serde(default, rename = "use_auto_tmm")]
    pub auto_tmm: Option<bool>,
    #[serde(default)]
    pub stopped: Option<bool>,
    #[serde(default)]
    pub stop_condition: Option<StopCondition>,
    #[serde(default)]
    pub skip_checking: bool,
    #[serde(default, rename = "add_to_top_of_queue")]
    pub add_to_queue_top: Option<bool>,
    #[serde(default)]
    pub content_layout: Option<ContentLayout>,
    #[serde(default = "minus_one")]
    pub upload_limit: i64,
    #[serde(default = "minus_one")]
    pub download_limit: i64,
    #[serde(default = "minus_two_f")]
    pub ratio_limit: f64,
    #[serde(default = "minus_two")]
    pub seeding_time_limit: i64,
    #[serde(default = "minus_two")]
    pub inactive_seeding_time_limit: i64,
}

impl Default for TorrentParamsTemplate {
    fn default() -> Self {
        Self {
            category: String::new(),
            tags: Vec::new(),
            save_path: String::new(),
            use_download_path: None,
            download_path: String::new(),
            auto_tmm: None,
            stopped: None,
            stop_condition: None,
            skip_checking: false,
            add_to_queue_top: None,
            content_layout: None,
            upload_limit: minus_one(),
            download_limit: minus_one(),
            ratio_limit: minus_two_f(),
            seeding_time_limit: minus_two(),
            inactive_seeding_time_limit: minus_two(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn minus_one() -> i64 {
    -1
}
fn minus_two() -> i64 {
    -2
}
fn minus_two_f() -> f64 {
    -2.0
}

/// RSS Downloader rule (`/api/v2/rss/rules` value).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct RssAutoDownloadRule {
    /// The rule name is the key of the `rss/rules` object, not a field.
    #[serde(skip)]
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub use_regex: bool,
    #[serde(default)]
    pub must_contain: String,
    #[serde(default)]
    pub must_not_contain: String,
    #[serde(default)]
    pub episode_filter: String,
    #[serde(default)]
    pub affected_feeds: Vec<String>,
    /// RFC 2822 date of the last match, empty if never.
    #[serde(default)]
    pub last_match: String,
    #[serde(default)]
    pub ignore_days: i64,
    #[serde(default)]
    pub smart_filter: bool,
    #[serde(default)]
    pub previously_matched_episodes: Vec<String>,
    #[serde(default)]
    pub torrent_params: TorrentParamsTemplate,
}

/// Articles a rule matches, grouped by feed (`/api/v2/rss/matchingArticles`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct RssMatchingArticles {
    pub feed_name: String,
    pub article_titles: Vec<String>,
}
