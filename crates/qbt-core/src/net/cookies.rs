//! Persistent cookie jar of the HTTP client (qBittorrent's
//! `DownloadManager::NetworkCookieJar`).
//!
//! Domain semantics follow Qt: a domain starting with `.` matches that
//! domain and all its subdomains; any other domain matches that host only
//! (host-only cookie). `Set-Cookie` without a `Domain` attribute creates a
//! host-only cookie; a `Domain` attribute must cover the request host and
//! may not be a bare top-level name. Cookies marked `Secure` are only sent
//! over https. Session cookies live in memory only; session and expired
//! cookies are dropped on load and save. The jar is bounded (per-cookie size,
//! per-domain and total counts) so a hostile server cannot grow it without
//! limit.

use std::net::IpAddr;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::error::{QbtError, QbtResult};
use crate::model::Cookie;

const MAX_COOKIE_BYTES: usize = 4096;
const MAX_PER_DOMAIN: usize = 50;
const MAX_TOTAL: usize = 3000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCookie {
    pub name: String,
    pub value: String,
    /// Lowercase; leading `.` = domain cookie, otherwise host-only.
    pub domain: String,
    pub path: String,
    /// Unix seconds, 0 = session cookie.
    pub expires: i64,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub http_only: bool,
    /// Insertion counter (oldest cookies are evicted first).
    #[serde(default)]
    pub created: u64,
}

impl StoredCookie {
    fn is_session(&self) -> bool {
        self.expires == 0
    }
    fn is_expired(&self, now: i64) -> bool {
        self.expires != 0 && self.expires <= now
    }
    fn same_identity(&self, other: &StoredCookie) -> bool {
        self.name == other.name && self.domain == other.domain && self.path == other.path
    }
    fn matches_host(&self, host: &str) -> bool {
        match self.domain.strip_prefix('.') {
            Some(d) => host == d || (host.len() > d.len() && host.ends_with(d) && host.as_bytes()[host.len() - d.len() - 1] == b'.'),
            None => host == self.domain,
        }
    }
}

#[derive(Debug, Default)]
pub struct CookieJar {
    cookies: Vec<StoredCookie>,
    counter: u64,
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_graphic() && !"()<>@,;:\\\"/[]?={}".contains(c)
}

fn valid_value(v: &str) -> bool {
    v.chars().all(|c| !c.is_control() && c != ';')
}

fn normalize_domain(d: &str) -> String {
    d.trim().to_ascii_lowercase()
}

/// Path matching (RFC 6265 §5.1.4).
fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/') || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Default cookie path: the request path up to (excluding) its last `/`.
fn default_path(url: &Url) -> String {
    let p = url.path();
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

impl CookieJar {
    /// Loads a saved jar (JSON array); session and expired cookies are dropped.
    pub fn from_json(data: &[u8], now: i64) -> Self {
        let mut cookies: Vec<StoredCookie> = serde_json::from_slice(data).unwrap_or_default();
        cookies.retain(|c| {
            !c.is_session()
                && !c.is_expired(now)
                && !c.name.is_empty()
                && c.name.len() + c.value.len() <= MAX_COOKIE_BYTES
                && c.name.chars().all(is_token_char)
                && valid_value(&c.value)
        });
        cookies.truncate(MAX_TOTAL);
        let counter = cookies.iter().map(|c| c.created).max().unwrap_or(0) + 1;
        Self { cookies, counter }
    }

    /// Persistent, unexpired cookies as JSON (what gets saved).
    pub fn to_json(&self, now: i64) -> Vec<u8> {
        let keep: Vec<&StoredCookie> = self.cookies.iter().filter(|c| !c.is_session() && !c.is_expired(now)).collect();
        serde_json::to_vec_pretty(&keep).unwrap_or_else(|_| b"[]".to_vec())
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// All cookies for the cookie manager / `app/cookies`.
    pub fn all(&self) -> Vec<Cookie> {
        self.cookies
            .iter()
            .map(|c| Cookie {
                name: c.name.clone(),
                domain: c.domain.clone(),
                path: c.path.clone(),
                value: c.value.clone(),
                expiration_date: c.expires,
            })
            .collect()
    }

    /// Replaces the whole jar (`app/setCookies`, cookie manager dialog).
    pub fn set_all(&mut self, cookies: Vec<Cookie>) -> QbtResult<()> {
        let mut out: Vec<StoredCookie> = Vec::with_capacity(cookies.len());
        if cookies.len() > MAX_TOTAL {
            return Err(QbtError::invalid(format!("too many cookies (the limit is {MAX_TOTAL})")));
        }
        for c in cookies {
            if c.name.is_empty() || !c.name.chars().all(is_token_char) {
                return Err(QbtError::invalid(format!("invalid cookie name: {:?}", c.name)));
            }
            if !valid_value(&c.value) || c.name.len() + c.value.len() > MAX_COOKIE_BYTES {
                return Err(QbtError::invalid(format!("invalid value for cookie {:?}", c.name)));
            }
            let domain = normalize_domain(&c.domain);
            if domain.is_empty() || domain == "." || !domain.chars().all(|ch| ch.is_ascii_alphanumeric() || "-._:[]".contains(ch)) {
                return Err(QbtError::invalid(format!("invalid domain for cookie {:?}: {:?}", c.name, c.domain)));
            }
            let path = if c.path.is_empty() { "/".to_string() } else { c.path };
            if !path.starts_with('/') || path.chars().any(|ch| ch.is_control() || ch == ';') {
                return Err(QbtError::invalid(format!("invalid path for cookie {:?}", c.name)));
            }
            let sc = StoredCookie {
                name: c.name,
                value: c.value,
                domain,
                path,
                expires: c.expiration_date.max(0),
                secure: false,
                http_only: false,
                created: self.counter,
            };
            self.counter += 1;
            out.retain(|o| !o.same_identity(&sc));
            out.push(sc);
        }
        self.cookies = out;
        Ok(())
    }

    /// `Cookie` header value for a request to `url`.
    pub fn header_for(&self, url: &Url, now: i64) -> Option<String> {
        let host = url.host_str()?.to_ascii_lowercase();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let secure = url.scheme() == "https";
        let path = url.path();
        let mut matching: Vec<&StoredCookie> = self
            .cookies
            .iter()
            .filter(|c| !c.is_expired(now) && c.matches_host(host) && path_matches(path, &c.path) && (!c.secure || secure))
            .collect();
        if matching.is_empty() {
            return None;
        }
        // Longer paths first, then older cookies (RFC 6265 §5.4).
        matching.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.created.cmp(&b.created)));
        Some(matching.iter().map(|c| format!("{}={}", c.name, c.value)).collect::<Vec<_>>().join("; "))
    }

    /// Stores the cookies of a response. Returns true when the persistent
    /// part of the jar changed (the caller saves it).
    pub fn store_response(&mut self, url: &Url, set_cookie: &[String], now: i64) -> bool {
        let mut changed = false;
        for header in set_cookie {
            if let Some(c) = self.parse_set_cookie(url, header, now) {
                changed |= self.insert(c, now);
            }
        }
        changed
    }

    fn insert(&mut self, c: StoredCookie, now: i64) -> bool {
        let mut changed = false;
        if let Some(pos) = self.cookies.iter().position(|o| o.same_identity(&c)) {
            let old = self.cookies.remove(pos);
            changed |= !old.is_session();
        }
        if c.is_expired(now) {
            // An expiry in the past deletes the cookie.
            return changed;
        }
        // Make room: expired ones first, then the oldest of the same domain.
        self.cookies.retain(|o| !o.is_expired(now));
        let same_domain = self.cookies.iter().filter(|o| o.domain == c.domain).count();
        if same_domain >= MAX_PER_DOMAIN
            && let Some(pos) = self
                .cookies
                .iter()
                .enumerate()
                .filter(|(_, o)| o.domain == c.domain)
                .min_by_key(|(_, o)| o.created)
                .map(|(i, _)| i)
        {
            let old = self.cookies.remove(pos);
            changed |= !old.is_session();
        }
        if self.cookies.len() >= MAX_TOTAL {
            return changed;
        }
        changed |= !c.is_session();
        self.cookies.push(c);
        changed
    }

    /// Parses one `Set-Cookie` header (RFC 6265 §5.2) received from `url`.
    fn parse_set_cookie(&mut self, url: &Url, header: &str, now: i64) -> Option<StoredCookie> {
        let host = url.host_str()?.to_ascii_lowercase();
        let host = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let mut parts = header.split(';');
        let (name, value) = parts.next()?.split_once('=')?;
        let name = name.trim();
        let value = value.trim().trim_matches('"');
        if name.is_empty() || !name.chars().all(is_token_char) || !valid_value(value) {
            return None;
        }
        if name.len() + value.len() > MAX_COOKIE_BYTES {
            return None;
        }
        let mut domain_attr: Option<String> = None;
        let mut path: Option<String> = None;
        let mut expires: Option<i64> = None;
        let mut max_age: Option<i64> = None;
        let mut secure = false;
        let mut http_only = false;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr.trim(), ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "domain" if !v.is_empty() => domain_attr = Some(normalize_domain(v.trim_start_matches('.'))),
                "path" if v.starts_with('/') => path = Some(v.to_string()),
                "expires" => expires = parse_cookie_date(v),
                "max-age" => {
                    if let Ok(n) = v.parse::<i64>() {
                        max_age = Some(n);
                    }
                }
                "secure" => secure = true,
                "httponly" => http_only = true,
                _ => {}
            }
        }
        if path.as_deref().is_some_and(|p| p.chars().any(char::is_control)) {
            return None;
        }
        let domain = match domain_attr {
            None => host.clone(),
            Some(d) => {
                let host_is_ip = host.parse::<IpAddr>().is_ok();
                if d == host {
                    if host_is_ip { host.clone() } else { format!(".{d}") }
                } else if !host_is_ip && host.ends_with(&format!(".{d}")) && d.contains('.') {
                    format!(".{d}")
                } else {
                    // Domain does not cover the request host, or is a bare
                    // top-level name: reject (no cross-site cookies).
                    return None;
                }
            }
        };
        let expires = match (max_age, expires) {
            (Some(n), _) if n <= 0 => 1, // already expired
            (Some(n), _) => now.saturating_add(n),
            (None, Some(t)) => t.max(1),
            (None, None) => 0,
        };
        let created = self.counter;
        self.counter += 1;
        Some(StoredCookie {
            name: name.to_string(),
            value: value.to_string(),
            domain,
            path: path.unwrap_or_else(|| default_path(url)),
            expires,
            secure,
            http_only,
            created,
        })
    }
}

/// Cookie date parsing per RFC 6265 §5.1.1 (accepts RFC 1123, RFC 850 and
/// asctime formats and the usual variations). Returns Unix seconds.
pub fn parse_cookie_date(s: &str) -> Option<i64> {
    let is_delim = |c: char| {
        c == '\t' || (' '..='/').contains(&c) || (';'..='@').contains(&c) || ('['..='`').contains(&c) || ('{'..='~').contains(&c)
    };
    let mut time: Option<(u32, u32, u32)> = None;
    let mut day: Option<u32> = None;
    let mut month: Option<u32> = None;
    let mut year: Option<i32> = None;
    for token in s.split(is_delim).filter(|t| !t.is_empty()) {
        if time.is_none() {
            let parts: Vec<&str> = token.split(':').collect();
            if parts.len() == 3 {
                let digits = |p: &str| {
                    let d: String = p.chars().take_while(char::is_ascii_digit).collect();
                    (!d.is_empty() && d.len() <= 2).then(|| d.parse::<u32>().ok()).flatten()
                };
                if let (Some(h), Some(m), Some(sec)) = (digits(parts[0]), digits(parts[1]), digits(parts[2])) {
                    time = Some((h, m, sec));
                    continue;
                }
            }
        }
        let lead: String = token.chars().take_while(char::is_ascii_digit).collect();
        if day.is_none() && (1..=2).contains(&lead.len()) {
            day = lead.parse().ok();
            continue;
        }
        // `get`: byte 3 need not be a char boundary (non-ASCII input).
        if month.is_none()
            && let Some(prefix) = token.get(..3)
        {
            const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
            let t = prefix.to_ascii_lowercase();
            if let Some(i) = MONTHS.iter().position(|m| *m == t) {
                month = Some(i as u32 + 1);
                continue;
            }
        }
        if year.is_none() && (2..=4).contains(&lead.len()) {
            year = lead.parse().ok();
            continue;
        }
    }
    let mut year = year?;
    if (70..=99).contains(&year) {
        year += 1900;
    } else if (0..=69).contains(&year) {
        year += 2000;
    }
    let (h, m, sec) = time?;
    if year < 1601 || h > 23 || m > 59 || sec > 59 {
        return None;
    }
    let date = chrono::NaiveDate::from_ymd_opt(year, month?, day?)?;
    Some(date.and_hms_opt(h, m, sec)?.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_760_000_000;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn cookie_dates_with_non_ascii_tokens_do_not_panic() {
        assert_eq!(parse_cookie_date("ab\u{e9} 2026 10:00:00"), None);
        assert_eq!(parse_cookie_date("\u{e9}\u{e9} \u{1F600}"), None);
        assert!(parse_cookie_date("Wed, 21 Oct 2026 07:28:00 GMT").is_some());
    }

    #[test]
    fn dates() {
        assert_eq!(parse_cookie_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
        assert_eq!(parse_cookie_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(784111777));
        assert_eq!(parse_cookie_date("Sun Nov  6 08:49:37 1994"), Some(784111777));
        assert_eq!(parse_cookie_date("Wed, 21-Oct-2026 07:28:00 GMT"), Some(1792567680));
        assert_eq!(parse_cookie_date("garbage"), None);
    }

    #[test]
    fn host_only_and_domain_scoping() {
        let mut jar = CookieJar::default();
        let changed = jar.store_response(
            &url("https://www.example.com/a/b"),
            &[
                "host=1".into(),
                "dom=2; Domain=example.com; Path=/; Max-Age=3600".into(),
                "evil=3; Domain=com".into(),
                "other=4; Domain=other.org".into(),
                "sec=5; Secure; Expires=Wed, 21-Oct-2099 07:28:00 GMT".into(),
                "bad\r\nname=6".into(),
            ],
            NOW,
        );
        assert!(changed);
        assert_eq!(jar.len(), 3);
        // Host-only cookie: same host only; default path "/a".
        // Longer paths first ("/a" before "/").
        assert_eq!(jar.header_for(&url("https://www.example.com/a/x"), NOW).unwrap(), "host=1; sec=5; dom=2");
        assert_eq!(jar.header_for(&url("https://example.com/a"), NOW).unwrap(), "dom=2");
        assert_eq!(jar.header_for(&url("https://api.example.com/"), NOW).unwrap(), "dom=2");
        // Not for look-alike domains, other sites or the parent TLD.
        assert_eq!(jar.header_for(&url("https://badexample.com/"), NOW), None);
        assert_eq!(jar.header_for(&url("https://other.org/"), NOW), None);
        // Secure cookie never over plain http; path scoping.
        assert_eq!(jar.header_for(&url("http://www.example.com/b"), NOW).unwrap(), "dom=2");
        assert_eq!(jar.header_for(&url("https://www.example.com/ab"), NOW).unwrap(), "dom=2");
        // Expiry.
        assert_eq!(jar.header_for(&url("https://example.com/"), NOW + 7200), None);
        // Deletion via Max-Age=0.
        jar.store_response(&url("https://example.com/"), &["dom=x; Domain=example.com; Path=/; Max-Age=0".into()], NOW);
        assert_eq!(jar.header_for(&url("https://api.example.com/"), NOW), None);
    }

    #[test]
    fn ip_hosts_and_limits() {
        let mut jar = CookieJar::default();
        jar.store_response(&url("http://192.168.1.10/"), &["a=1; Domain=168.1.10".into(), "b=2; Domain=192.168.1.10".into()], NOW);
        assert_eq!(jar.header_for(&url("http://192.168.1.10/"), NOW).unwrap(), "b=2");
        let many: Vec<String> = (0..MAX_PER_DOMAIN + 10).map(|i| format!("c{i}=v; Max-Age=100")).collect();
        jar.store_response(&url("http://flood.test/"), &many, NOW);
        assert_eq!(jar.all().iter().filter(|c| c.domain == "flood.test").count(), MAX_PER_DOMAIN);
        let huge = format!("big={}", "x".repeat(MAX_COOKIE_BYTES));
        assert!(!jar.store_response(&url("http://flood.test/"), &[huge], NOW));
    }

    #[test]
    fn persistence_drops_session_and_expired() {
        let mut jar = CookieJar::default();
        jar.store_response(
            &url("https://site.test/"),
            &["session=1".into(), "keep=2; Max-Age=1000".into(), "short=3; Max-Age=10".into()],
            NOW,
        );
        assert_eq!(jar.len(), 3);
        let saved = jar.to_json(NOW);
        let loaded = CookieJar::from_json(&saved, NOW + 100);
        let names: Vec<String> = loaded.all().into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["keep"]);
        assert_eq!(CookieJar::from_json(b"not json", NOW).len(), 0);
    }

    #[test]
    fn set_all_validates() {
        let mut jar = CookieJar::default();
        let ok = Cookie {
            name: "sid".into(),
            domain: ".Example.com".into(),
            path: "".into(),
            value: "abc".into(),
            expiration_date: NOW + 100,
        };
        jar.set_all(vec![ok.clone()]).unwrap();
        let all = jar.all();
        assert_eq!(all[0].domain, ".example.com");
        assert_eq!(all[0].path, "/");
        assert_eq!(jar.header_for(&url("https://sub.example.com/x"), NOW).unwrap(), "sid=abc");
        for bad in [
            Cookie { name: "".into(), ..ok.clone() },
            Cookie { name: "a b".into(), ..ok.clone() },
            Cookie { value: "x\r\nInjected: 1".into(), ..ok.clone() },
            Cookie { domain: "exa mple.com".into(), ..ok.clone() },
            Cookie { path: "nopath".into(), ..ok.clone() },
        ] {
            assert!(jar.set_all(vec![bad]).is_err());
        }
        // A failed update leaves the jar untouched.
        assert_eq!(jar.len(), 1);
    }
}
