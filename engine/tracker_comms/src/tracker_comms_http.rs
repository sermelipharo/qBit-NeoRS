//! HTTP(S) tracker protocol (BEP 3, 7, 23, 48).

use std::fmt::Write;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use librqbit_core::hash_id::Id20;
use url::Url;

use crate::error::TrackerError;

/// Maximum size of an HTTP tracker response body.
pub const MAX_RESPONSE_SIZE: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnnounceEvent {
    None,
    Started,
    Completed,
    Stopped,
}

impl AnnounceEvent {
    pub fn as_str(&self) -> Option<&'static str> {
        match self {
            AnnounceEvent::None => None,
            AnnounceEvent::Started => Some("started"),
            AnnounceEvent::Completed => Some("completed"),
            AnnounceEvent::Stopped => Some("stopped"),
        }
    }

    pub(crate) fn udp_code(&self) -> u32 {
        use crate::tracker_comms_udp::*;
        match self {
            AnnounceEvent::None => EVENT_NONE,
            AnnounceEvent::Started => EVENT_STARTED,
            AnnounceEvent::Completed => EVENT_COMPLETED,
            AnnounceEvent::Stopped => EVENT_STOPPED,
        }
    }
}

/// Parameters of one announce, independent of the transport.
#[derive(Clone, Debug)]
pub struct AnnounceRequest {
    pub info_hash: Id20,
    pub peer_id: Id20,
    pub port: u16,
    pub uploaded: u64,
    pub downloaded: u64,
    pub left: u64,
    pub event: AnnounceEvent,
    pub key: u32,
    pub num_want: Option<u32>,
    /// Address to report as `ip=` (announce IP). `None` in anonymous mode.
    pub ip: Option<IpAddr>,
    pub tracker_id: Option<Vec<u8>>,
}

impl AnnounceRequest {
    /// The announce query parameters (without a leading `?`).
    pub fn as_querystring(&self) -> String {
        use urlencoding as u;
        let mut s = String::with_capacity(256);
        s.push_str("info_hash=");
        s.push_str(u::encode_binary(&self.info_hash.0).as_ref());
        s.push_str("&peer_id=");
        s.push_str(u::encode_binary(&self.peer_id.0).as_ref());
        let _ = write!(
            s,
            "&port={}&uploaded={}&downloaded={}&left={}&corrupt=0&key={:08X}",
            self.port, self.uploaded, self.downloaded, self.left, self.key
        );
        if let Some(event) = self.event.as_str() {
            let _ = write!(s, "&event={event}");
        }
        if let Some(numwant) = self.num_want {
            let _ = write!(s, "&numwant={numwant}");
        }
        s.push_str("&compact=1&no_peer_id=1");
        if let Some(ip) = &self.ip {
            let _ = write!(s, "&ip={}", u::encode(&ip.to_string()));
        }
        if let Some(trackerid) = &self.tracker_id {
            s.push_str("&trackerid=");
            s.push_str(u::encode_binary(trackerid).as_ref());
        }
        s
    }
}

/// Append `params` to the tracker URL's own query string (if any), dropping
/// any fragment.
pub fn url_with_params(tracker: &Url, params: &str) -> Url {
    let mut url = tracker.clone();
    url.set_fragment(None);
    let query = match tracker.query() {
        Some(q) if !q.is_empty() => format!("{q}&{params}"),
        _ => params.to_owned(),
    };
    url.set_query(Some(&query));
    url
}

/// Derive the scrape URL of an HTTP announce URL (BEP 48): the last path
/// component must start with `announce`, which is replaced by `scrape`.
pub fn scrape_url(announce: &Url) -> Option<Url> {
    if !matches!(announce.scheme(), "http" | "https") {
        return None;
    }
    let path = announce.path();
    let slash = path.rfind('/')?;
    let last = &path[slash + 1..];
    let rest = last.strip_prefix("announce")?;
    let mut url = announce.clone();
    url.set_path(&format!("{}scrape{}", &path[..=slash], rest));
    url.set_fragment(None);
    Some(url)
}

/// Query string of an HTTP scrape request.
pub fn scrape_querystring(info_hash: &Id20) -> String {
    format!(
        "info_hash={}",
        urlencoding::encode_binary(&info_hash.0).as_ref()
    )
}

fn describe_reqwest_error(e: reqwest::Error) -> TrackerError {
    // Never include the URL: it may carry a passkey.
    let e = e.without_url();
    let mut msg = if e.is_timeout() {
        "tracker timed out".to_owned()
    } else if e.is_redirect() {
        "redirect refused".to_owned()
    } else if e.is_connect() {
        "could not connect to tracker".to_owned()
    } else {
        "HTTP request failed".to_owned()
    };
    let mut source = std::error::Error::source(&e);
    let mut depth = 0;
    while let Some(s) = source {
        let text = s.to_string();
        if !text.is_empty() && !text.contains("://") && depth < 4 {
            msg.push_str(": ");
            msg.push_str(&text);
        }
        depth += 1;
        source = s.source();
    }
    TrackerError::other(crate::response::sanitize_message(msg.as_bytes()))
}

/// GET `url` and return the body (capped at [`MAX_RESPONSE_SIZE`]) and the
/// address that answered.
pub(crate) async fn http_get(
    client: &reqwest::Client,
    url: Url,
    user_agent: Option<&str>,
    timeout: Duration,
) -> Result<(Vec<u8>, Option<SocketAddr>), TrackerError> {
    let mut req = client.get(url).timeout(timeout);
    if let Some(ua) = user_agent
        && let Ok(v) = reqwest::header::HeaderValue::from_str(ua)
    {
        req = req.header(reqwest::header::USER_AGENT, v);
    }
    let mut response = req.send().await.map_err(describe_reqwest_error)?;
    let remote = response.remote_addr();
    let status = response.status();
    if !status.is_success() {
        let e = TrackerError::other(format!("tracker responded with HTTP {status}"));
        // 410 Gone: never talk to this tracker again (libtorrent behaviour).
        return Err(if status == reqwest::StatusCode::GONE {
            e.permanent()
        } else {
            e
        });
    }
    if response
        .content_length()
        .is_some_and(|l| l > MAX_RESPONSE_SIZE as u64)
    {
        return Err(TrackerError::other("tracker response is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(describe_reqwest_error)? {
        if body.len() + chunk.len() > MAX_RESPONSE_SIZE {
            return Err(TrackerError::other("tracker response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, remote))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> AnnounceRequest {
        AnnounceRequest {
            info_hash: Id20::new([
                1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
            ]),
            peer_id: Id20::new(*b"-qB5240-abcdefghijkl"),
            port: 6881,
            uploaded: 10,
            downloaded: 20,
            left: 1024,
            event: AnnounceEvent::Started,
            key: 0xdead,
            num_want: Some(200),
            ip: Some("1.2.3.4".parse().unwrap()),
            tracker_id: Some(b"t&d".to_vec()),
        }
    }

    #[test]
    fn querystring() {
        let q = req().as_querystring();
        assert_eq!(
            q,
            "info_hash=%01%02%03%04%05%06%07%08%09%0A%0B%0C%0D%0E%0F%10%11%12%13%14\
             &peer_id=-qB5240-abcdefghijkl&port=6881&uploaded=10&downloaded=20&left=1024\
             &corrupt=0&key=0000DEAD&event=started&numwant=200&compact=1&no_peer_id=1\
             &ip=1.2.3.4&trackerid=t%26d"
        );
        let mut r = req();
        r.event = AnnounceEvent::None;
        r.ip = Some("::1".parse().unwrap());
        r.tracker_id = None;
        r.num_want = None;
        let q = r.as_querystring();
        assert!(!q.contains("event="));
        assert!(!q.contains("numwant="));
        assert!(q.ends_with("&ip=%3A%3A1"));
    }

    #[test]
    fn url_params_keep_existing_query() {
        let u = Url::parse("http://t.example/announce.php?passkey=abc#frag").unwrap();
        let full = url_with_params(&u, "info_hash=%01");
        assert_eq!(
            full.as_str(),
            "http://t.example/announce.php?passkey=abc&info_hash=%01"
        );
        let u = Url::parse("http://t.example/announce").unwrap();
        assert_eq!(
            url_with_params(&u, "a=%FF").as_str(),
            "http://t.example/announce?a=%FF"
        );
    }

    #[test]
    fn scrape_urls() {
        let s = |u: &str| scrape_url(&Url::parse(u).unwrap()).map(|u| u.to_string());
        assert_eq!(
            s("http://example.com/announce").as_deref(),
            Some("http://example.com/scrape")
        );
        assert_eq!(
            s("http://example.com/x/announce").as_deref(),
            Some("http://example.com/x/scrape")
        );
        assert_eq!(
            s("http://example.com/announce.php").as_deref(),
            Some("http://example.com/scrape.php")
        );
        assert_eq!(
            s("http://example.com/a/announce?x2%0644").as_deref(),
            Some("http://example.com/a/scrape?x2%0644")
        );
        assert_eq!(s("https://example.com/announce/abc").as_deref(), None);
        assert_eq!(s("http://example.com/a").as_deref(), None);
        assert_eq!(s("http://example.com/x%064announce").as_deref(), None);
        assert_eq!(s("http://example.com/announce/").as_deref(), None);
        assert_eq!(s("udp://example.com:80/announce").as_deref(), None);
        assert_eq!(s("http://example.com/xannounce").as_deref(), None);
    }
}
