//! Feeds and their articles (qBittorrent `RSS::Feed`, `RSS::Article`,
//! `FeedSerializer`): article insertion rules, read state, trimming to
//! `rss_max_articles_per_feed`, and the `articles/<uid>.json` format.

use std::collections::HashSet;
use std::fmt;

use serde_json::{Map, Value};
use tokio::time::Instant;

use super::date::{format_rfc2822, parse_rfc2822};
use super::parser::{ParsedArticle, starts_with_ci, truncate};
use crate::model::{RssArticle, RssFeed};

/// Feed identifier (qBittorrent `QUuid`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Uid(pub u128);

impl Uid {
    /// Random version-4 UUID.
    pub fn generate() -> Self {
        let mut v: u128 = rand::random();
        v &= !(0xF000u128 << 64);
        v |= 0x4000u128 << 64; // version 4
        v &= !(0xC000u128 << 48);
        v |= 0x8000u128 << 48; // RFC 4122 variant
        Uid(v)
    }

    /// Accepts `{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}` with or without braces.
    /// The nil UUID is invalid (like a null `QUuid`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('{').and_then(|r| r.strip_suffix('}')).unwrap_or(s);
        let b = s.as_bytes();
        if b.len() != 36 || b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
            return None;
        }
        let hex: String = s.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u128::from_str_radix(&hex, 16).ok()?;
        (v != 0).then_some(Uid(v))
    }

    /// 32 lowercase hex digits: the article/icon file stem.
    pub fn hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

impl fmt::Display for Uid {
    /// `QUuid::toString()`: braced, lowercase.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let h = self.hex();
        write!(f, "{{{}-{}-{}-{}-{}}}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Article {
    pub id: String,
    /// Unix ms.
    pub date: i64,
    pub title: String,
    pub author: String,
    pub description: String,
    pub torrent_url: String,
    pub link: String,
    pub is_read: bool,
    pub extra: Vec<(String, String)>,
}

const ARTICLE_KEYS: [&str; 8] = ["id", "date", "title", "author", "description", "torrentURL", "link", "isRead"];

impl Article {
    pub fn from_parsed(p: ParsedArticle, date: i64) -> Self {
        Self {
            id: p.id,
            date,
            title: p.title,
            author: p.author,
            description: p.description,
            torrent_url: p.torrent_url,
            link: p.link,
            is_read: false,
            extra: p.extra,
        }
    }

    /// `Article::torrentUrl()`: the torrent URL, else the link.
    pub fn torrent_url_or_link(&self) -> &str {
        if self.torrent_url.is_empty() { &self.link } else { &self.torrent_url }
    }

    pub fn to_dto(&self) -> RssArticle {
        RssArticle {
            id: self.id.clone(),
            date: self.date.div_euclid(1000),
            title: self.title.clone(),
            author: self.author.clone(),
            description: self.description.clone(),
            torrent_url: self.torrent_url.clone(),
            link: self.link.clone(),
            is_read: self.is_read,
        }
    }

    /// The article data map (storage and WebAPI `rss/items`).
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        for (k, v) in &self.extra {
            m.insert(k.clone(), Value::String(v.clone()));
        }
        m.insert("id".into(), self.id.clone().into());
        m.insert("date".into(), format_rfc2822(self.date).into());
        m.insert("title".into(), self.title.clone().into());
        m.insert("author".into(), self.author.clone().into());
        m.insert("description".into(), self.description.clone().into());
        m.insert("torrentURL".into(), self.torrent_url.clone().into());
        m.insert("link".into(), self.link.clone().into());
        m.insert("isRead".into(), self.is_read.into());
        Value::Object(m)
    }

    pub fn from_json(obj: &Map<String, Value>) -> Self {
        let text = |k: &str| match obj.get(k) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => String::new(),
        };
        let is_read = match obj.get("isRead") {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
            Some(Value::String(s)) => s.eq_ignore_ascii_case("true") || s == "1",
            _ => false,
        };
        let extra = obj
            .iter()
            .filter(|(k, _)| !ARTICLE_KEYS.contains(&k.as_str()))
            .take(32)
            .map(|(k, v)| {
                let v = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                (k.clone(), truncate(v, 64 * 1024))
            })
            .collect();
        Self {
            id: text("id"),
            date: parse_rfc2822(&text("date")).unwrap_or(0),
            title: text("title"),
            author: text("author"),
            description: text("description"),
            torrent_url: text("torrentURL"),
            link: text("link"),
            is_read,
            extra,
        }
    }
}

/// A feed and its in-memory state.
#[derive(Debug)]
pub(crate) struct Feed {
    pub uid: Uid,
    pub url: String,
    /// Full item path (`Folder\Name`).
    pub path: String,
    /// Seconds; 0 = global interval.
    pub refresh_interval: i64,
    pub title: String,
    /// Remembered `<lastBuildDate>`/`<updated>` (memory only).
    pub last_build_date: String,
    pub is_loading: bool,
    pub has_error: bool,
    /// Newest first.
    pub articles: Vec<Article>,
    ids: HashSet<String>,
    pub unread: usize,
    /// Articles need saving.
    pub dirty: bool,
    /// Next scheduled refresh; `None` = due (the epoch in qBittorrent).
    pub next_refresh: Option<Instant>,
    /// Incremented on every refresh; stale downloads are ignored.
    pub generation: u64,
    /// The favicon download was attempted this session.
    pub icon_attempted: bool,
    /// `<uid>.ico` exists.
    pub icon_loaded: bool,
}

impl Feed {
    pub fn new(uid: Uid, url: String, path: String, refresh_interval: i64) -> Self {
        Self {
            uid,
            url,
            path,
            refresh_interval: refresh_interval.max(0),
            title: String::new(),
            last_build_date: String::new(),
            is_loading: false,
            has_error: false,
            articles: Vec::new(),
            ids: HashSet::new(),
            unread: 0,
            dirty: false,
            next_refresh: None,
            generation: 0,
            icon_attempted: false,
            icon_loaded: false,
        }
    }

    pub fn name(&self) -> &str {
        super::tree::relative_name(&self.path)
    }

    pub fn article(&self, id: &str) -> Option<&Article> {
        if !self.ids.contains(id) {
            return None;
        }
        self.articles.iter().find(|a| a.id == id)
    }

    /// `handleArticleLoadFinished`: articles from storage (already sorted).
    pub fn set_loaded_articles(&mut self, mut articles: Vec<Article>, max: usize) {
        articles.truncate(max);
        self.articles.clear();
        self.ids.clear();
        self.unread = 0;
        for a in articles {
            if !self.ids.insert(a.id.clone()) {
                continue;
            }
            if !a.is_read {
                self.unread += 1;
            }
            self.articles.push(a);
        }
    }

    /// `Feed::updateArticles`: merges a parse result. Returns the number of
    /// new articles counted like qBittorrent and the ids that were inserted.
    pub fn update_articles(&mut self, loaded: Vec<ParsedArticle>, max: usize, now_ms: i64) -> (usize, Vec<String>) {
        if loaded.is_empty() {
            return (0, Vec::new());
        }
        let mut dummy_date = now_ms;
        let mut new_articles = Vec::with_capacity(loaded.len());
        for p in loaded {
            if let Some(existing) = self.article(&p.id) {
                // Date-less articles must not jump ahead of known ones.
                dummy_date = existing.date - 1;
                continue;
            }
            let date = p.date.unwrap_or(dummy_date);
            new_articles.push(Article::from_parsed(p, date));
        }
        if new_articles.is_empty() {
            return (0, Vec::new());
        }

        // (date, index into new_articles or None for an existing article)
        let mut sort_data: Vec<(i64, Option<usize>)> = self.articles.iter().map(|a| (a.date, None)).collect();
        sort_data.extend(new_articles.iter().enumerate().map(|(i, a)| (a.date, Some(i))));
        sort_data.sort_by_key(|a| std::cmp::Reverse(a.0));
        sort_data.truncate(max);

        let mut slots: Vec<Option<Article>> = new_articles.into_iter().map(Some).collect();
        let mut count = 0;
        let mut added = Vec::new();
        for (_, idx) in sort_data.iter().rev() {
            if let Some(i) = idx
                && let Some(article) = slots[*i].take()
            {
                let id = article.id.clone();
                if self.add_article(article, max) {
                    added.push(id);
                }
                count += 1;
            }
        }
        added.retain(|id| self.ids.contains(id));
        (count, added)
    }

    /// `Feed::addArticle`: insertion by date, newest first.
    fn add_article(&mut self, article: Article, max: usize) -> bool {
        let pos = self.articles.partition_point(|a| a.date > article.date);
        if pos >= max {
            return false;
        }
        if !article.is_read {
            self.unread += 1;
        }
        self.ids.insert(article.id.clone());
        self.articles.insert(pos, article);
        self.dirty = true;
        if self.articles.len() > max {
            self.remove_oldest();
        }
        true
    }

    fn remove_oldest(&mut self) {
        if let Some(oldest) = self.articles.pop() {
            self.ids.remove(&oldest.id);
            if !oldest.is_read {
                self.unread = self.unread.saturating_sub(1);
            }
        }
    }

    /// `rss_max_articles_per_feed` lowered: drops the oldest articles.
    pub fn trim(&mut self, max: usize) -> bool {
        let mut changed = false;
        while self.articles.len() > max {
            self.remove_oldest();
            changed = true;
        }
        changed
    }

    pub fn mark_all_read(&mut self) -> bool {
        let mut changed = false;
        for a in &mut self.articles {
            if !a.is_read {
                a.is_read = true;
                changed = true;
            }
        }
        if changed {
            self.unread = 0;
            self.dirty = true;
        }
        changed
    }

    pub fn mark_read(&mut self, id: &str) -> bool {
        if !self.ids.contains(id) {
            return false;
        }
        match self.articles.iter_mut().find(|a| a.id == id) {
            Some(a) if !a.is_read => {
                a.is_read = true;
                self.unread = self.unread.saturating_sub(1);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    pub fn to_dto(&self, with_articles: bool, icon_path: String) -> RssFeed {
        RssFeed {
            uid: self.uid.to_string(),
            url: self.url.clone(),
            path: self.path.clone(),
            title: self.title.clone(),
            last_build_date: self.last_build_date.clone(),
            is_loading: self.is_loading,
            has_error: self.has_error,
            refresh_interval: self.refresh_interval,
            unread_count: self.unread as i64,
            articles: if with_articles { self.articles.iter().map(Article::to_dto).collect() } else { Vec::new() },
            icon_path,
        }
    }

    /// `Feed::toJsonValue(withData)` (WebAPI `rss/items`).
    pub fn to_api_json(&self, with_data: bool) -> Value {
        let mut m = Map::new();
        m.insert("uid".into(), self.uid.to_string().into());
        m.insert("url".into(), self.url.clone().into());
        if self.refresh_interval > 0 {
            m.insert("refreshInterval".into(), self.refresh_interval.into());
        }
        if with_data {
            m.insert("title".into(), self.title.clone().into());
            m.insert("lastBuildDate".into(), self.last_build_date.clone().into());
            m.insert("isLoading".into(), self.is_loading.into());
            m.insert("hasError".into(), self.has_error.into());
            m.insert("articles".into(), Value::Array(self.articles.iter().map(Article::to_json).collect()));
        }
        Value::Object(m)
    }

    /// Compact JSON array for `articles/<uid>.json`.
    pub fn articles_file(&self) -> Vec<u8> {
        let arr = Value::Array(self.articles.iter().map(Article::to_json).collect());
        serde_json::to_vec(&arr).unwrap_or_else(|_| b"[]".to_vec())
    }
}

/// `FeedSerializer::loadArticles`: sorted newest first (stable).
pub(crate) fn load_articles(data: &[u8], url: &str) -> (Vec<Article>, Vec<String>) {
    let mut warnings = Vec::new();
    let value: Value = match serde_json::from_slice(data) {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("Couldn't parse RSS Session data. Error: {e}"));
            return (Vec::new(), warnings);
        }
    };
    let Value::Array(items) = value else {
        warnings.push("Couldn't load RSS Session data. Invalid data format.".to_owned());
        return (Vec::new(), warnings);
    };
    let mut out = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        match item {
            Value::Object(obj) => out.push(Article::from_json(obj)),
            _ => warnings.push(format!("Couldn't load RSS article '{url}#{i}'. Invalid data format.")),
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.date));
    (out, warnings)
}

/// `Article::isSupportedTorrentURL`: HTTP(S), magnet or a bare info hash.
pub(crate) fn is_supported_torrent_url(url: &str) -> bool {
    starts_with_ci(url, "http:") || starts_with_ci(url, "https:") || starts_with_ci(url, "magnet:") || is_info_hash(url)
}

/// `TorrentDescriptor::parse` succeeds: a magnet URI with a BitTorrent
/// exact topic, or a bare v1 (40 hex / 32 base32) or v2 (64 hex) info hash.
pub(crate) fn is_torrent_descriptor(url: &str) -> bool {
    if starts_with_ci(url, "magnet:") {
        let lower = url.to_ascii_lowercase();
        return lower.contains("xt=urn:btih:") || lower.contains("xt=urn:btmh:");
    }
    is_info_hash(url)
}

fn is_info_hash(s: &str) -> bool {
    let hex = |n| s.len() == n && s.bytes().all(|c| c.is_ascii_hexdigit());
    hex(40) || hex(64) || (s.len() == 32 && s.bytes().all(|c| matches!(c.to_ascii_uppercase(), b'A'..=b'Z' | b'2'..=b'7')))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(id: &str, date: Option<i64>) -> ParsedArticle {
        ParsedArticle { id: id.into(), title: id.into(), date, ..Default::default() }
    }

    #[test]
    fn uid_format_and_parse() {
        let uid = Uid::generate();
        let s = uid.to_string();
        assert_eq!(s.len(), 38);
        assert!(s.starts_with('{') && s.ends_with('}'));
        assert_eq!(Uid::parse(&s), Some(uid));
        assert_eq!(Uid::parse(&s[1..37]), Some(uid));
        assert_eq!(uid.hex().len(), 32);
        assert_eq!(&s[15..16], "4");
        assert_eq!(Uid::parse("{00000000-0000-0000-0000-000000000000}"), None);
        assert_eq!(Uid::parse("{6b8c0000-0000-0000-0000-00000000000g}"), None);
        assert_eq!(Uid::parse("../../etc/passwd"), None);
        let fixed = Uid::parse("{6B8C1234-0000-4000-8000-0000000000AB}").unwrap();
        assert_eq!(fixed.to_string(), "{6b8c1234-0000-4000-8000-0000000000ab}");
        assert_eq!(fixed.hex(), "6b8c12340000400080000000000000ab");
    }

    #[test]
    fn update_articles_orders_dates_and_caps() {
        let mut feed = Feed::new(Uid::generate(), "u".into(), "u".into(), 0);
        // Parser order is reverse document order: oldest-in-doc first.
        let (n, added) = feed.update_articles(vec![parsed("c", Some(3000)), parsed("b", Some(2000)), parsed("a", Some(1000))], 50, 10_000);
        assert_eq!(n, 3);
        assert_eq!(added.len(), 3);
        let ids: Vec<_> = feed.articles.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
        assert_eq!(feed.unread, 3);

        // Known articles are skipped; a date-less new article placed after a
        // known one gets "known date - 1 ms".
        let (n, _) = feed.update_articles(vec![parsed("b", None), parsed("x", None), parsed("d", Some(5000))], 50, 10_000);
        assert_eq!(n, 2);
        assert_eq!(feed.article("x").unwrap().date, 1999);
        let ids: Vec<_> = feed.articles.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["d", "c", "b", "x", "a"]);

        // Capacity: only the newest survive.
        let (_, _) = feed.update_articles(vec![parsed("old", Some(1))], 5, 10_000);
        assert!(feed.article("old").is_none());
        assert_eq!(feed.articles.len(), 5);
        assert!(feed.trim(2));
        let ids: Vec<_> = feed.articles.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["d", "c"]);
        assert_eq!(feed.unread, 2);

        // Max 0 keeps nothing.
        let mut empty = Feed::new(Uid::generate(), "u".into(), "u".into(), 0);
        empty.update_articles(vec![parsed("z", Some(1))], 0, 0);
        assert!(empty.articles.is_empty());
    }

    #[test]
    fn read_state_and_storage_round_trip() {
        let mut feed = Feed::new(Uid::generate(), "u".into(), "u".into(), 0);
        let mut p = parsed("a", Some(1_791_000_306_000));
        p.extra.push(("category".into(), "Linux".into()));
        p.torrent_url = "https://e/a.torrent".into();
        feed.update_articles(vec![parsed("b", Some(1_791_000_000_000)), p], 50, 0);
        assert!(feed.mark_read("a"));
        assert!(!feed.mark_read("a"));
        assert!(!feed.mark_read("missing"));
        assert_eq!(feed.unread, 1);

        let data = feed.articles_file();
        let (loaded, warnings) = load_articles(&data, "u");
        assert!(warnings.is_empty());
        assert_eq!(loaded, feed.articles);
        let mut other = Feed::new(Uid::generate(), "u".into(), "u".into(), 0);
        other.set_loaded_articles(loaded, 1);
        assert_eq!(other.articles.len(), 1);
        assert_eq!(other.unread, 0);

        assert!(feed.mark_all_read());
        assert_eq!(feed.unread, 0);
        assert!(!feed.mark_all_read());

        let (none, warnings) = load_articles(b"{\"not\":\"array\"}", "u");
        assert!(none.is_empty() && warnings.len() == 1);
        let (some, warnings) = load_articles(b"[1, {\"id\":\"x\",\"date\":\"bad\"}]", "u");
        assert_eq!(some.len(), 1);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn api_json_shape() {
        let mut feed = Feed::new(Uid::parse("{6b8c1234-0000-4000-8000-0000000000ab}").unwrap(), "https://e/rss".into(), "F".into(), 0);
        let v = feed.to_api_json(false);
        assert_eq!(v, serde_json::json!({"uid": "{6b8c1234-0000-4000-8000-0000000000ab}", "url": "https://e/rss"}));
        feed.refresh_interval = 900;
        feed.update_articles(vec![parsed("a", Some(1_791_000_306_000))], 50, 0);
        let v = feed.to_api_json(true);
        assert_eq!(v["refreshInterval"], 900);
        assert_eq!(v["isLoading"], false);
        assert_eq!(v["articles"][0]["date"], "03 Oct 2026 04:05:06 +0000");
        assert_eq!(v["articles"][0]["isRead"], false);
        assert_eq!(v["articles"][0]["torrentURL"], "");
    }

    #[test]
    fn torrent_url_support() {
        assert!(is_supported_torrent_url("https://e/x.torrent"));
        assert!(is_supported_torrent_url("HTTP://e/x.torrent"));
        assert!(is_supported_torrent_url("magnet:?xt=urn:btih:abc"));
        assert!(is_supported_torrent_url("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_supported_torrent_url("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"));
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/plain,x", "ftp://e/x", "", "/tmp/x.torrent"] {
            assert!(!is_supported_torrent_url(bad), "{bad}");
        }
        assert!(is_torrent_descriptor("magnet:?dn=x&xt=urn:btih:abc"));
        assert!(!is_torrent_descriptor("magnet:?dn=x"));
        assert!(!is_torrent_descriptor("https://e/x.torrent"));
    }
}
