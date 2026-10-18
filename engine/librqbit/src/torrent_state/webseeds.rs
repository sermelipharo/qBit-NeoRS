//! Per-torrent web seed list (BEP 19 `url-list`) and per-seed status.
//!
//! The list lives in `ManagedTorrentShared`, so it survives pause/unpause and
//! can be edited in every state. The live torrent's web seed scheduler
//! (`live/webseed.rs`) reads it and reports back through it.

use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

use bencode::BencodeValue;
use buffers::ByteBuf;
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::Notify;
use url::{Host, Url};

use super::ManagedTorrent;

/// At most this many web seeds per torrent.
pub(crate) const MAX_WEB_SEEDS: usize = 100;
/// Longest accepted web seed URL.
const MAX_URL_LEN: usize = 4096;
/// A web seed is banned after this many pieces that failed the hash check.
pub(crate) const BAN_AFTER_HASH_FAILURES: u32 = 3;
/// Window of the download rate estimate.
const RATE_WINDOW: Duration = Duration::from_secs(5);

/// State of one web seed (qBittorrent's "HTTP Sources").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum WebSeedState {
    /// Usable, nothing being fetched right now.
    Idle,
    /// A piece is being fetched.
    Downloading,
    /// Waiting after an error (or the server's `Retry-After`).
    Backoff,
    /// Disabled after repeated hash failures.
    Banned,
    /// Never contacted (invalid or refused URL).
    Error,
}

/// Status of one web seed.
#[derive(Debug, Clone, Serialize)]
pub struct WebSeedStatus {
    pub url: String,
    pub state: WebSeedState,
    /// Last error or the reason for `Banned`/`Error`.
    pub message: Option<String>,
    /// Verified payload bytes downloaded from this seed.
    pub downloaded: u64,
    /// Bytes/s over the last 5 seconds.
    pub download_rate: u64,
    /// Pieces from this seed that failed the hash check.
    pub hash_failures: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WebSeedEditError {
    #[error("web seed not found")]
    NotFound,
    #[error("invalid web seed URL")]
    InvalidUrl,
    #[error("web seed already exists")]
    AlreadyExists,
}

/// Outcome of one fetch attempt, reported by the live scheduler.
#[derive(Debug)]
pub(crate) enum FetchOutcome {
    /// A piece was downloaded and verified.
    Ok { bytes: u64 },
    /// Nothing to download right now.
    NoPiece,
    /// The piece was completed by someone else meanwhile.
    Wasted,
    /// The piece failed the hash check.
    HashFailed,
    /// The server asked us to come back later.
    RetryAfter(Duration),
    /// Network/HTTP error.
    Failed(String),
}

struct Entry {
    id: u32,
    url: String,
    // Added by the user or from a trusted (local) torrent: the SSRF rules
    // for torrent-supplied seeds don't apply.
    trusted: bool,
    state: WebSeedState,
    message: Option<String>,
    downloaded: u64,
    hash_failures: u32,
    // Consecutive failures, for the backoff.
    failures: u32,
    retry_at: Option<Instant>,
    samples: VecDeque<(Instant, u64)>,
}

impl Entry {
    fn rate(&self, now: Instant) -> u64 {
        let bytes: u64 = self
            .samples
            .iter()
            .filter(|(t, _)| now.saturating_duration_since(*t) < RATE_WINDOW)
            .map(|(_, b)| *b)
            .sum();
        bytes / RATE_WINDOW.as_secs()
    }

    fn status(&self, now: Instant) -> WebSeedStatus {
        WebSeedStatus {
            url: self.url.clone(),
            state: self.state,
            message: self.message.clone(),
            downloaded: self.downloaded,
            download_rate: self.rate(now),
            hash_failures: self.hash_failures,
        }
    }
}

#[derive(Default)]
struct Inner {
    seeds: Vec<Entry>,
    next_id: u32,
}

/// The web seeds of a torrent.
#[derive(Default)]
pub struct TorrentWebSeeds {
    inner: Mutex<Inner>,
    /// Signalled when the list changes.
    pub(crate) changed: Notify,
}

impl std::fmt::Debug for TorrentWebSeeds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TorrentWebSeeds")
            .field("len", &self.inner.lock().seeds.len())
            .finish()
    }
}

/// Parses and validates a web seed URL (http/https, bounded length).
fn parse_url(s: &str) -> Option<Url> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_URL_LEN {
        return None;
    }
    let url = Url::parse(s).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return None;
    }
    Some(url)
}

/// SSRF mitigation (like libtorrent's): a web seed supplied by the torrent
/// that points to a local address and carries a query string is not
/// contacted. User-added seeds are trusted.
pub(crate) fn ssrf_refused(url: &Url) -> bool {
    if url.query().is_none() {
        return false;
    }
    match url.host() {
        Some(Host::Ipv4(ip)) => tracker_comms::is_local_ip(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => tracker_comms::is_local_ip(IpAddr::V6(ip)),
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        None => true,
    }
}

/// The pseudo-peer handle of web seed `id` in the piece tracker: an address in
/// the IPv6 discard prefix `100::/64` (RFC 6666), port 0, so it never clashes
/// with a real peer.
pub(crate) fn pseudo_peer_addr(id: u32) -> SocketAddr {
    #[allow(clippy::cast_possible_truncation)]
    let ip = Ipv6Addr::new(
        0x0100,
        0,
        0,
        0,
        0x7765,
        0x6273,
        (id >> 16) as u16,
        id as u16,
    );
    SocketAddr::new(IpAddr::V6(ip), 0)
}

/// `url-list` of a torrent file: a single string or a list of strings.
pub(crate) fn url_list_from_torrent(torrent_bytes: &[u8]) -> Vec<String> {
    let Ok(BencodeValue::Dict(d)) = bencode::dyn_from_bytes::<ByteBuf>(torrent_bytes) else {
        return Vec::new();
    };
    let as_string = |v: &BencodeValue<ByteBuf>| match v {
        BencodeValue::Bytes(b) => std::str::from_utf8(b.as_ref())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty()),
        _ => None,
    };
    match d.get(&ByteBuf(b"url-list")) {
        Some(BencodeValue::List(l)) => l.iter().filter_map(as_string).take(MAX_WEB_SEEDS).collect(),
        Some(v) => as_string(v).into_iter().collect(),
        None => Vec::new(),
    }
}

/// `ws` parameters of a magnet link.
pub(crate) fn web_seeds_from_magnet(magnet: &str) -> Vec<String> {
    Url::parse(magnet)
        .map(|u| {
            u.query_pairs()
                .filter(|(k, _)| k == "ws")
                .map(|(_, v)| v.into_owned())
                .take(MAX_WEB_SEEDS)
                .collect()
        })
        .unwrap_or_default()
}

fn backoff(failures: u32) -> Duration {
    // 30 s, 60 s, 120 s ... at most an hour (libtorrent waits 30 s).
    let exp = failures.saturating_sub(1).min(7);
    Duration::from_secs((30u64 << exp).min(3600))
}

impl TorrentWebSeeds {
    /// `untrusted` come from the torrent (SSRF rule applies), `trusted` from the user.
    pub(crate) fn new(untrusted: Vec<String>, trusted: Vec<String>) -> Self {
        let s = Self::default();
        {
            let mut g = s.inner.lock();
            for u in untrusted {
                Self::add_locked(&mut g, &u, false);
            }
            for u in trusted {
                Self::add_locked(&mut g, &u, true);
            }
        }
        s
    }

    /// Seeds in this order, each `(url, trusted)`.
    pub(crate) fn with_trust(list: Vec<(String, bool)>) -> Self {
        let s = Self::default();
        {
            let mut g = s.inner.lock();
            for (u, trusted) in list {
                Self::add_locked(&mut g, &u, trusted);
            }
        }
        s
    }

    fn add_locked(g: &mut Inner, url: &str, trusted: bool) -> bool {
        if g.seeds.len() >= MAX_WEB_SEEDS {
            return false;
        }
        let parsed = parse_url(url);
        let url = parsed
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| url.trim().to_owned());
        if url.is_empty() || url.len() > MAX_URL_LEN || g.seeds.iter().any(|e| e.url == url) {
            return false;
        }
        let (state, message) = match &parsed {
            None => (WebSeedState::Error, Some("invalid URL".to_owned())),
            Some(u) if !trusted && ssrf_refused(u) => (
                WebSeedState::Error,
                Some("refused: local address with a query string".to_owned()),
            ),
            Some(_) => (WebSeedState::Idle, None),
        };
        let id = g.next_id;
        g.next_id = g.next_id.wrapping_add(1);
        g.seeds.push(Entry {
            id,
            url,
            trusted,
            state,
            message,
            downloaded: 0,
            hash_failures: 0,
            failures: 0,
            retry_at: None,
            samples: VecDeque::new(),
        });
        true
    }

    pub fn statuses(&self) -> Vec<WebSeedStatus> {
        let now = Instant::now();
        self.inner
            .lock()
            .seeds
            .iter()
            .map(|e| e.status(now))
            .collect()
    }

    pub fn urls(&self) -> Vec<String> {
        self.inner
            .lock()
            .seeds
            .iter()
            .map(|e| e.url.clone())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().seeds.is_empty()
    }

    /// Adds user (trusted) web seeds; invalid URLs and duplicates are skipped.
    pub fn add(&self, urls: Vec<String>) -> usize {
        let added = {
            let mut g = self.inner.lock();
            urls.iter()
                .filter(|u| parse_url(u).is_some() && Self::add_locked(&mut g, u, true))
                .count()
        };
        if added > 0 {
            self.changed.notify_waiters();
        }
        added
    }

    pub fn remove(&self, urls: &[String]) -> usize {
        let removed = {
            let mut g = self.inner.lock();
            let before = g.seeds.len();
            g.seeds.retain(|e| {
                !urls
                    .iter()
                    .any(|u| u.trim() == e.url || canonical(u) == e.url)
            });
            before - g.seeds.len()
        };
        if removed > 0 {
            self.changed.notify_waiters();
        }
        removed
    }

    /// Replaces `old` by `new` (a fresh, trusted seed at the same position).
    pub fn edit(&self, old: &str, new: &str) -> Result<(), WebSeedEditError> {
        {
            let mut g = self.inner.lock();
            let old = canonical(old);
            let pos = g
                .seeds
                .iter()
                .position(|e| e.url == old || e.url == old.trim())
                .ok_or(WebSeedEditError::NotFound)?;
            let new = parse_url(new).ok_or(WebSeedEditError::InvalidUrl)?;
            let new = new.to_string();
            if g.seeds.iter().any(|e| e.url == new) {
                return Err(WebSeedEditError::AlreadyExists);
            }
            let id = g.next_id;
            g.next_id = g.next_id.wrapping_add(1);
            g.seeds[pos] = Entry {
                id,
                url: new,
                trusted: true,
                state: WebSeedState::Idle,
                message: None,
                downloaded: 0,
                hash_failures: 0,
                failures: 0,
                retry_at: None,
                samples: VecDeque::new(),
            };
        }
        self.changed.notify_waiters();
        Ok(())
    }

    /// Replaces the whole list with trusted seeds (e.g. a persisted list).
    pub fn replace(&self, urls: Vec<String>) {
        {
            let mut g = self.inner.lock();
            g.seeds.clear();
            for u in urls {
                Self::add_locked(&mut g, &u, true);
            }
        }
        self.changed.notify_waiters();
    }

    /// Seeds ready to fetch now, `(id, url, trusted)`; they are marked
    /// `Downloading`.
    #[cfg(test)]
    pub(crate) fn take_ready(&self, now: Instant) -> Vec<(u32, String, bool)> {
        self.take_ready_limited(now, usize::MAX)
    }

    /// At most `limit` ready seeds.
    pub(crate) fn take_ready_limited(&self, now: Instant, limit: usize) -> Vec<(u32, String, bool)> {
        let mut g = self.inner.lock();
        g.seeds
            .iter_mut()
            .filter(|e| match e.state {
                WebSeedState::Idle => e.retry_at.is_none_or(|t| t <= now),
                WebSeedState::Backoff => e.retry_at.is_none_or(|t| t <= now),
                _ => false,
            })
            .take(limit)
            .map(|e| {
                e.state = WebSeedState::Downloading;
                (e.id, e.url.clone(), e.trusted)
            })
            .collect()
    }

    /// When the next waiting seed becomes ready (None: nothing waits).
    pub(crate) fn next_wakeup(&self) -> Option<Instant> {
        self.inner
            .lock()
            .seeds
            .iter()
            .filter(|e| matches!(e.state, WebSeedState::Idle | WebSeedState::Backoff))
            .map(|e| e.retry_at.unwrap_or_else(Instant::now))
            .min()
    }

    /// Whether seed `id` is still in the list.
    pub(crate) fn contains(&self, id: u32) -> bool {
        self.inner.lock().seeds.iter().any(|e| e.id == id)
    }

    /// Puts every seed that was fetching back to `Idle` (the live state ended).
    pub(crate) fn reset_downloading(&self) {
        for e in self.inner.lock().seeds.iter_mut() {
            if e.state == WebSeedState::Downloading {
                e.state = WebSeedState::Idle;
            }
        }
    }

    /// Applies the outcome of a fetch by seed `id` (ignored if it was removed).
    pub(crate) fn finish(&self, id: u32, outcome: FetchOutcome) {
        let now = Instant::now();
        let mut g = self.inner.lock();
        let Some(e) = g.seeds.iter_mut().find(|e| e.id == id) else {
            return;
        };
        if e.state != WebSeedState::Downloading {
            return;
        }
        e.state = WebSeedState::Idle;
        e.retry_at = None;
        match outcome {
            FetchOutcome::Ok { bytes } => {
                e.failures = 0;
                e.message = None;
                e.downloaded += bytes;
                while e
                    .samples
                    .front()
                    .is_some_and(|(t, _)| now.saturating_duration_since(*t) >= RATE_WINDOW)
                {
                    e.samples.pop_front();
                }
                e.samples.push_back((now, bytes));
            }
            FetchOutcome::NoPiece | FetchOutcome::Wasted => {
                // Nothing to do right now: look again shortly.
                e.retry_at = Some(now + Duration::from_secs(1));
            }
            FetchOutcome::HashFailed => {
                e.hash_failures += 1;
                if e.hash_failures >= BAN_AFTER_HASH_FAILURES {
                    e.state = WebSeedState::Banned;
                    e.message = Some(format!(
                        "banned after {} pieces failed the hash check",
                        e.hash_failures
                    ));
                } else {
                    e.message = Some("piece failed the hash check".to_owned());
                }
            }
            FetchOutcome::RetryAfter(d) => {
                e.state = WebSeedState::Backoff;
                e.message = Some(format!("server busy, retrying in {}s", d.as_secs()));
                e.retry_at = Some(now + d);
            }
            FetchOutcome::Failed(msg) => {
                e.failures += 1;
                let d = backoff(e.failures);
                e.state = WebSeedState::Backoff;
                e.message = Some(msg);
                e.retry_at = Some(now + d);
            }
        }
    }
}

fn canonical(s: &str) -> String {
    parse_url(s)
        .map(|u| u.to_string())
        .unwrap_or_else(|| s.trim().to_owned())
}

impl ManagedTorrent {
    /// The torrent's web seeds (BEP 19) with their status.
    pub fn web_seeds(&self) -> Vec<WebSeedStatus> {
        self.shared.web_seeds.statuses()
    }

    /// Web seed URLs, for persistence (restore with
    /// `AddTorrentOptions::replace_web_seeds`).
    pub fn web_seed_urls(&self) -> Vec<String> {
        self.shared.web_seeds.urls()
    }

    /// Adds web seeds (trusted: the SSRF rule doesn't apply). Invalid URLs
    /// (not http/https) and duplicates are skipped. Returns how many were added.
    pub fn add_web_seeds(&self, urls: Vec<String>) -> usize {
        self.shared.web_seeds.add(urls)
    }

    /// Removes web seeds; a piece being fetched from one is given back.
    pub fn remove_web_seeds(&self, urls: &[String]) -> usize {
        self.shared.web_seeds.remove(urls)
    }

    /// Replaces the whole web seed list (trusted seeds, status starts over).
    pub fn replace_web_seeds(&self, urls: Vec<String>) {
        self.shared.web_seeds.replace(urls)
    }

    /// Replaces a web seed URL (its status starts over).
    pub fn edit_web_seed(&self, old: &str, new: &str) -> Result<(), WebSeedEditError> {
        self.shared.web_seeds.edit(old, new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_list_string_and_list() {
        let single = b"d4:infod4:name1:ae8:url-list16:http://a/b/c.isoe";
        assert_eq!(url_list_from_torrent(single), vec!["http://a/b/c.iso"]);
        let list = b"d4:infod4:name1:ae8:url-listl9:http://x/9:http://y/i5e0:ee";
        assert_eq!(
            url_list_from_torrent(list),
            vec!["http://x/".to_owned(), "http://y/".to_owned()]
        );
        assert!(url_list_from_torrent(b"d4:infod4:name1:aee").is_empty());
        assert!(url_list_from_torrent(b"garbage").is_empty());
    }

    #[test]
    fn magnet_ws() {
        let m = "magnet:?xt=urn:btih:0000000000000000000000000000000000000000&ws=http%3A%2F%2Fh%2Ff&ws=https://g/";
        assert_eq!(web_seeds_from_magnet(m), vec!["http://h/f", "https://g/"]);
    }

    #[test]
    fn ssrf_rule() {
        let refused = |s: &str| ssrf_refused(&Url::parse(s).unwrap());
        assert!(refused("http://127.0.0.1/x?a=b"));
        assert!(refused("http://192.168.1.2:8080/x?a=b"));
        assert!(refused("http://[::1]/x?a"));
        assert!(refused("http://[fe80::1]/x?a"));
        assert!(refused("http://localhost/x?a"));
        assert!(!refused("http://127.0.0.1/x"));
        assert!(!refused("http://example.com/x?a=b"));
        assert!(!refused("http://8.8.8.8/x?a=b"));

        let ws = TorrentWebSeeds::new(
            vec!["http://10.0.0.1/f?k=v".into(), "ftp://x/".into()],
            vec!["http://10.0.0.2/f?k=v".into()],
        );
        let st = ws.statuses();
        assert_eq!(st.len(), 3);
        assert_eq!(st[0].state, WebSeedState::Error);
        assert_eq!(st[1].state, WebSeedState::Error);
        assert_eq!(st[2].state, WebSeedState::Idle);
        // Errored seeds are never handed out.
        let ready = ws.take_ready(Instant::now());
        assert_eq!(ready.len(), 1);
    }

    #[test]
    fn edit_add_remove_and_ban() {
        let ws = TorrentWebSeeds::new(vec![], vec![]);
        assert_eq!(
            ws.add(vec!["http://a/".into(), "http://a/".into(), "nope".into()]),
            1
        );
        assert_eq!(
            ws.edit("http://b/", "http://c/"),
            Err(WebSeedEditError::NotFound)
        );
        assert_eq!(
            ws.edit("http://a/", "bad"),
            Err(WebSeedEditError::InvalidUrl)
        );
        ws.add(vec!["http://c/".into()]);
        assert_eq!(
            ws.edit("http://a/", "http://c/"),
            Err(WebSeedEditError::AlreadyExists)
        );
        ws.edit("http://a/", "http://d/").unwrap();
        assert_eq!(ws.urls(), vec!["http://d/", "http://c/"]);

        let ready = ws.take_ready(Instant::now());
        assert_eq!(ready.len(), 2);
        let (id, _, trusted) = ready[0].clone();
        assert!(trusted);
        for _ in 0..BAN_AFTER_HASH_FAILURES {
            ws.inner.lock().seeds[0].state = WebSeedState::Downloading;
            ws.finish(id, FetchOutcome::HashFailed);
        }
        assert_eq!(ws.statuses()[0].state, WebSeedState::Banned);
        ws.finish(ready[1].0, FetchOutcome::Failed("boom".into()));
        let st = ws.statuses();
        assert_eq!(st[1].state, WebSeedState::Backoff);
        assert!(ws.take_ready(Instant::now()).is_empty());

        assert_eq!(ws.remove(&["http://d/".into()]), 1);
        assert!(!ws.contains(id));
    }

    #[test]
    fn trust_is_per_seed_and_kept_in_order() {
        let ws = TorrentWebSeeds::with_trust(vec![
            ("http://10.0.0.1/a?k=v".into(), false),
            ("http://10.0.0.1/b?k=v".into(), true),
            ("http://example.com/c".into(), false),
        ]);
        assert_eq!(
            ws.urls(),
            vec!["http://10.0.0.1/a?k=v", "http://10.0.0.1/b?k=v", "http://example.com/c"]
        );
        let st = ws.statuses();
        assert_eq!(st[0].state, WebSeedState::Error);
        assert_eq!(st[1].state, WebSeedState::Idle);
        let ready = ws.take_ready(Instant::now());
        let trust: Vec<bool> = ready.iter().map(|r| r.2).collect();
        assert_eq!(trust, vec![true, false]);
        // An edit is a user action: the new seed is trusted.
        ws.edit("http://example.com/c", "http://10.0.0.9/c?k=v").unwrap();
        assert_eq!(ws.statuses()[2].state, WebSeedState::Idle);
    }

    #[test]
    fn ready_seeds_are_limited() {
        let ws = TorrentWebSeeds::with_trust((0..10).map(|i| (format!("http://h{i}/"), true)).collect());
        assert_eq!(ws.take_ready_limited(Instant::now(), 4).len(), 4);
        assert_eq!(ws.take_ready_limited(Instant::now(), 0).len(), 0);
        assert_eq!(ws.take_ready(Instant::now()).len(), 6);
    }

    #[test]
    fn pseudo_addrs_unique() {
        assert_ne!(pseudo_peer_addr(1), pseudo_peer_addr(2));
        assert_ne!(pseudo_peer_addr(1), pseudo_peer_addr(1 << 16));
    }
}
