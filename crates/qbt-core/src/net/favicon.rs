//! Tracker favicons for the macOS Trackers filter (qBittorrent's
//! "Download tracker's favicon" option, `TrackerFiltersList`).
//!
//! For a tracker URL the icon is `<scheme>://<host2>/favicon.ico` where the
//! scheme is the tracker's own http(s) scheme (plain http otherwise) and
//! `<host2>` is the IP address, or the last two labels of the host name. If
//! that fails, `favicon.png` is tried. Requests use the shared HTTP client
//! (general-purpose proxy, certificate checks, redirects only to http(s)),
//! never exceed [`MAX_FAVICON_SIZE`] bytes and give up after [`TIMEOUT_SECS`].
//! The bytes are returned unparsed; decoding is the front end's job.

use std::net::IpAddr;

use reqwest::Url;

use super::http::{self, HttpPurpose, HttpRequest};
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};

/// Larger icons are rejected (real favicons are a few KiB).
pub const MAX_FAVICON_SIZE: u64 = 64 * 1024;
const TIMEOUT_SECS: u64 = 15;

/// Candidate icon URLs (`.ico` first, then `.png`) for a tracker URL, or
/// `None` when it has no usable host.
pub fn favicon_urls(tracker_url: &str) -> Option<[String; 2]> {
    let url = Url::parse(tracker_url.trim()).ok()?;
    let scheme = if url.scheme().starts_with("http") { url.scheme() } else { "http" };
    let host = url.host_str()?.trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    let host2 = match host.trim_matches(['[', ']']).parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => format!("[{ip}]"),
        Ok(IpAddr::V4(ip)) => ip.to_string(),
        Err(_) => {
            let labels: Vec<&str> = host.split('.').collect();
            labels[labels.len().saturating_sub(2)..].join(".")
        }
    };
    Some([format!("{scheme}://{host2}/favicon.ico"), format!("{scheme}://{host2}/favicon.png")])
}

/// Downloads the favicon of a tracker. Returns the raw image bytes.
pub async fn fetch_favicon(ctx: &CoreContext, tracker_url: &str) -> QbtResult<Vec<u8>> {
    let urls = favicon_urls(tracker_url).ok_or_else(|| QbtError::invalid("tracker URL has no host"))?;
    let mut last = QbtError::network("favicon not available");
    for url in urls {
        match fetch_one(ctx, url).await {
            Ok(body) => return Ok(body),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn fetch_one(ctx: &CoreContext, url: String) -> QbtResult<Vec<u8>> {
    let req = HttpRequest { url, cookie: None, max_size: MAX_FAVICON_SIZE, timeout_secs: TIMEOUT_SECS };
    let resp = http::fetch(ctx, req, HttpPurpose::General).await?;
    if resp.is_magnet_redirect() || !(200..300).contains(&resp.status) {
        return Err(QbtError::network("favicon not available"));
    }
    if resp.body.is_empty() || resp.body.len() as u64 > MAX_FAVICON_SIZE {
        return Err(QbtError::network("invalid favicon size"));
    }
    // HTML error pages served with 200 are not icons.
    if resp.content_type.to_ascii_lowercase().starts_with("text/") {
        return Err(QbtError::network("favicon is not an image"));
    }
    Ok(resp.body)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;
    use crate::net::testutil;

    #[test]
    fn urls_use_last_two_labels() {
        let u = favicon_urls("udp://tracker.opentrackr.org:1337/announce").unwrap();
        assert_eq!(u[0], "http://opentrackr.org/favicon.ico");
        assert_eq!(u[1], "http://opentrackr.org/favicon.png");
        let u = favicon_urls("https://a.b.example.org/announce?x=1").unwrap();
        assert_eq!(u[0], "https://example.org/favicon.ico");
        let u = favicon_urls("http://example/announce").unwrap();
        assert_eq!(u[0], "http://example/favicon.ico");
    }

    #[test]
    fn urls_keep_ip_hosts_and_reject_garbage() {
        assert_eq!(favicon_urls("http://10.1.2.3:6969/announce").unwrap()[0], "http://10.1.2.3/favicon.ico");
        assert_eq!(favicon_urls("udp://[2001:db8::1]:80/announce").unwrap()[0], "http://[2001:db8::1]/favicon.ico");
        assert!(favicon_urls("not a url").is_none());
        assert!(favicon_urls("").is_none());
    }

    async fn serve(body: &'static [u8], content_type: &'static str) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf).await;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(body).await;
                });
            }
        });
        port
    }

    #[test]
    fn fetch_validates_the_response() {
        let rt = testutil::runtime();
        let dir = tempfile::tempdir().unwrap();
        let ctx = testutil::context(&rt, dir.path());
        rt.block_on(async {
            let ok = serve(b"\x89PNG....", "image/png").await;
            let got = fetch_one(&ctx, format!("http://127.0.0.1:{ok}/favicon.ico")).await.unwrap();
            assert_eq!(got, b"\x89PNG....");
            let html = serve(b"<html>nope</html>", "text/html").await;
            assert!(fetch_one(&ctx, format!("http://127.0.0.1:{html}/favicon.ico")).await.is_err());
            static BIG: [u8; 70_000] = [7; 70_000];
            let big = serve(&BIG, "image/x-icon").await;
            assert!(fetch_one(&ctx, format!("http://127.0.0.1:{big}/favicon.ico")).await.is_err());
        });
    }
}
