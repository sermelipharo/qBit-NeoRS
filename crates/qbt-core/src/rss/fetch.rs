//! Feed and favicon downloads: the shared HTTP client with the RSS proxy
//! profile, size and time limits, and qBittorrent's "same host request
//! delay" (sequential service per `host:port`; the next request starts
//! `rss_fetch_delay` seconds after the previous one finished).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

use super::parser::MAX_FEED_SIZE;
use crate::context::CoreContext;
use crate::net::http::{self, HttpPurpose, HttpRequest};

const FEED_TIMEOUT_SECS: u64 = 60;
pub(crate) const MAX_ICON_SIZE: usize = 1024 * 1024;

type Slot = Arc<tokio::sync::Mutex<Option<Instant>>>;

#[derive(Default)]
pub(crate) struct HostQueue {
    slots: Mutex<HashMap<String, Slot>>,
}

impl HostQueue {
    /// Runs `fut` when the service is free and `delay` has passed since its
    /// previous request finished.
    pub async fn run<T>(&self, key: &str, delay: Duration, fut: impl Future<Output = T>) -> T {
        let slot = self.slots.lock().entry(key.to_owned()).or_default().clone();
        let mut last = slot.lock().await;
        if let Some(t) = *last {
            let ready = t + delay;
            if ready > Instant::now() {
                tokio::time::sleep_until(ready).await;
            }
        }
        let out = fut.await;
        *last = Some(Instant::now());
        drop(last);
        drop(slot);
        // Forget idle services whose delay has elapsed.
        self.slots.lock().retain(|_, s| {
            Arc::strong_count(s) > 1 || s.try_lock().map(|g| (*g).is_some_and(|t| t.elapsed() < delay)).unwrap_or(true)
        });
        out
    }
}

/// `ServiceID::fromURL`: host and port (80 when absent, like qBittorrent).
pub(crate) fn service_key(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => format!("{}:{}", u.host_str().unwrap_or_default(), u.port().unwrap_or(80)),
        Err(_) => url.to_owned(),
    }
}

/// Only `http`/`https` feed URLs are fetched.
pub(crate) fn validate_feed_url(url: &str) -> Result<(), String> {
    match reqwest::Url::parse(url) {
        Ok(u) if (u.scheme() == "http" || u.scheme() == "https") && u.host_str().is_some_and(|h| !h.is_empty()) => Ok(()),
        _ => Err(format!("Unsupported RSS feed URL: {url}. Only HTTP(S) URLs are supported.")),
    }
}

pub(crate) struct Downloaded {
    /// Final URL after redirects.
    pub url: String,
    pub body: Vec<u8>,
}

/// Downloads `url` with the RSS proxy profile; non-2xx statuses are errors.
pub(crate) async fn download(ctx: &CoreContext, url: &str, max_size: usize) -> Result<Downloaded, String> {
    validate_feed_url(url)?;
    let req = HttpRequest { url: url.to_owned(), cookie: None, max_size: max_size as u64, timeout_secs: FEED_TIMEOUT_SECS };
    let resp = http::fetch(ctx, req, HttpPurpose::Rss).await.map_err(|e| match e {
        crate::error::QbtError::Network { message } => message,
        other => other.to_string(),
    })?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("Error transferring {url} - server replied: HTTP {}", resp.status));
    }
    if resp.body.len() > max_size {
        return Err(format!("The downloaded data exceeds the {max_size} bytes limit"));
    }
    Ok(Downloaded { url: if resp.url.is_empty() { url.to_owned() } else { resp.url }, body: resp.body })
}

pub(crate) async fn download_feed(ctx: &CoreContext, url: &str) -> Result<Downloaded, String> {
    download(ctx, url, MAX_FEED_SIZE).await
}

/// `<scheme>://<host>/favicon.ico` of a feed URL.
pub(crate) fn icon_url(feed_url: &str) -> Option<String> {
    let u = reqwest::Url::parse(feed_url).ok()?;
    if u.scheme() != "http" && u.scheme() != "https" {
        return None;
    }
    Some(format!("{}://{}/favicon.ico", u.scheme(), u.host_str()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(service_key("https://example.org/rss"), "example.org:80");
        assert_eq!(service_key("http://example.org:8080/rss"), "example.org:8080");
        assert!(validate_feed_url("https://example.org/rss").is_ok());
        for bad in ["file:///etc/passwd", "ftp://x/y", "javascript:alert(1)", "example.org/rss", "", "http://"] {
            assert!(validate_feed_url(bad).is_err(), "{bad}");
        }
        assert_eq!(icon_url("https://example.org:8443/a/b?c").as_deref(), Some("https://example.org/favicon.ico"));
        assert_eq!(icon_url("file:///x"), None);
    }

    #[tokio::test]
    async fn same_host_requests_are_spaced() {
        let q = HostQueue::default();
        let delay = Duration::from_millis(300);
        let start = Instant::now();
        q.run("h:80", delay, async {}).await;
        q.run("h:80", delay, async {}).await;
        assert!(start.elapsed() >= delay);
        let t = Instant::now();
        q.run("other:80", delay, async {}).await;
        assert!(t.elapsed() < delay, "other hosts are not delayed");
    }
}
