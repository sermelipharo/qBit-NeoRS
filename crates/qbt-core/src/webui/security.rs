//! Request security checks (qBittorrent `WebApplication`): CSRF
//! (Origin/Referer vs target origin), Host header validation, reverse-proxy
//! client address resolution, authentication bypass decisions and the
//! prebuilt security / custom response headers. All functions are pure.

use std::net::{IpAddr, SocketAddr};

use http::{HeaderName, HeaderValue};

use super::subnet::{Subnet, any_contains, parse_ip};

/// Host and explicit port of a URL-ish value, like `QUrl::host()`/`port()`
/// (host lower-cased, IPv6 without brackets, port `None` when absent).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct HostPort {
    pub host: String,
    pub port: Option<u16>,
}

/// Parses `scheme://user@host:port/path` or a bare `host:port` (a Host
/// header). Malformed values give `None` (an invalid `QUrl`).
pub fn parse_authority(value: &str) -> Option<HostPort> {
    let v = value.trim();
    let rest = match v.find("://") {
        Some(i) => &v[i + 3..],
        None => v,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    if authority.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let close = after.find(']')?;
        let host = &after[..close];
        host.parse::<std::net::Ipv6Addr>().ok()?;
        let tail = &after[close + 1..];
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':')?),
        };
        (host, port)
    } else {
        match authority.split_once(':') {
            Some((h, p)) => {
                if p.contains(':') {
                    return None;
                }
                (h, Some(p))
            }
            None => (authority, None),
        }
    };
    let port = match port {
        None | Some("") => None,
        Some(p) => {
            if p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(p.parse::<u16>().ok()?)
        }
    };
    Some(HostPort { host: host.to_ascii_lowercase(), port })
}

/// `urlFromHostHeader` + host/port: an invalid value compares like an empty
/// `QUrl` (empty host, no port).
fn origin_of(value: &str) -> HostPort {
    parse_authority(value).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrfVerdict {
    SameSite,
    OriginMismatch,
    RefererMismatch,
}

/// qBittorrent's `isCrossSiteRequest`: no Origin and no Referer is allowed;
/// otherwise the Origin (or, if absent, the Referer) must have the same host
/// and port as the target origin (the scheme is not compared).
pub fn check_csrf(target: &str, origin: Option<&str>, referer: Option<&str>) -> CsrfVerdict {
    let origin = origin.filter(|s| !s.is_empty());
    let referer = referer.filter(|s| !s.is_empty());
    let target = origin_of(target);
    match (origin, referer) {
        (None, None) => CsrfVerdict::SameSite,
        (Some(o), _) => {
            if origin_of(o) == target {
                CsrfVerdict::SameSite
            } else {
                CsrfVerdict::OriginMismatch
            }
        }
        (None, Some(r)) => {
            if origin_of(r) == target {
                CsrfVerdict::SameSite
            } else {
                CsrfVerdict::RefererMismatch
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostVerdict {
    Valid,
    PortMismatch,
    Invalid,
}

/// qBittorrent's `validateHostHeader`. Domain patterns are wildcards
/// (`*`, `?`), case-insensitive and matched against the whole host name
/// (qBittorrent searches unanchored; anchoring is a deliberate hardening).
pub fn validate_host(host_header: &str, local: SocketAddr, domains: &[String]) -> HostVerdict {
    let hp = origin_of(host_header);
    if let Some(port) = hp.port
        && port != local.port()
    {
        return HostVerdict::PortMismatch;
    }
    if let Some(ip) = parse_ip(&hp.host)
        && ip == local.ip().to_canonical()
    {
        return HostVerdict::Valid;
    }
    if domains.iter().any(|d| wildcard_match(d, &hp.host)) {
        HostVerdict::Valid
    } else {
        HostVerdict::Invalid
    }
}

/// Case-insensitive, anchored wildcard match (`*` any run, `?` one char).
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.trim().chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Client address used for bans, whitelists and logs. Forwarded headers are
/// honoured only when the socket peer is a trusted reverse proxy, and
/// `X-Forwarded-For` is walked right to left skipping trusted proxies, so a
/// client cannot spoof its address by prepending entries (qBittorrent takes
/// the first global entry, which a client controls).
pub fn resolve_client(peer: IpAddr, forwarded_for: Option<&str>, trusted: Option<&[Subnet]>) -> IpAddr {
    let peer = peer.to_canonical();
    let Some(trusted) = trusted else { return peer };
    if !any_contains(trusted, peer) {
        return peer;
    }
    let Some(xff) = forwarded_for else { return peer };
    let mut last_trusted = None;
    for entry in xff.rsplit(',').map(str::trim).filter(|e| !e.is_empty()) {
        let Some(ip) = parse_forwarded_ip(entry) else { break };
        if any_contains(trusted, ip) {
            last_trusted = Some(ip);
            continue;
        }
        return ip;
    }
    last_trusted.unwrap_or(peer)
}

/// `1.2.3.4`, `1.2.3.4:5678`, `[::1]`, `[::1]:5678` or `::1`.
fn parse_forwarded_ip(entry: &str) -> Option<IpAddr> {
    if let Some(ip) = parse_ip(entry) {
        return Some(ip);
    }
    if let Ok(sa) = entry.parse::<SocketAddr>() {
        return Some(sa.ip().to_canonical());
    }
    None
}

/// Inputs of the authentication bypass decision.
pub struct BypassPolicy<'a> {
    /// `!bypass_local_auth`.
    pub local_auth_required: bool,
    pub whitelist_enabled: bool,
    pub whitelist: &'a [Subnet],
    /// The request carries reverse-proxy forwarding headers
    /// ([`has_forwarding_headers`]).
    pub proxied: bool,
}

/// Headers a reverse proxy adds when it forwards a request. A local proxy
/// connects from loopback whatever the real client is, so a request carrying
/// any of them never gets the localhost bypass (the qBittorrent class of
/// "auth bypass behind a reverse proxy").
const FORWARDING_HEADERS: [&str; 7] =
    ["x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-forwarded-server", "x-real-ip", "forwarded", "via"];

pub fn has_forwarding_headers(headers: &http::HeaderMap) -> bool {
    FORWARDING_HEADERS.iter().any(|h| headers.contains_key(*h))
}

/// qBittorrent's `isAuthNeeded`. Localhost bypass requires the real socket
/// peer *and* the resolved client to be loopback (IPv4-mapped IPv6 loopback
/// included) and no forwarding headers, so neither forwarded headers nor a
/// local reverse proxy (which always connects from loopback) can grant it.
pub fn is_auth_needed(policy: &BypassPolicy<'_>, socket_peer: IpAddr, client: IpAddr) -> bool {
    let peer = socket_peer.to_canonical();
    let client = client.to_canonical();
    if !policy.local_auth_required && !policy.proxied && peer.is_loopback() && client.is_loopback() {
        return false;
    }
    if policy.whitelist_enabled && any_contains(policy.whitelist, client) {
        return false;
    }
    true
}

/// DNS-rebinding guard for the authentication bypass (localhost bypass and
/// subnet whitelist). A web page can make the victim's browser talk to the
/// WebUI by rebinding its own host name to `127.0.0.1` or a LAN address;
/// such requests are same-origin for the browser (so the CSRF check passes)
/// and, with the default `*` server-domain list, pass Host validation too.
/// The bypass is therefore only granted when the Host header names the
/// server in a way a foreign site cannot: an IP literal, `localhost`, or a
/// host matching an explicitly configured (not catch-all) server domain.
pub fn host_allows_bypass(host_header: &str, domains: &[String]) -> bool {
    let Some(hp) = parse_authority(host_header) else { return false };
    if hp.host.is_empty() {
        return false;
    }
    if parse_ip(&hp.host).is_some() || hp.host == "localhost" {
        return true;
    }
    domains
        .iter()
        .map(|d| d.trim())
        .filter(|d| d.chars().any(|c| !matches!(c, '*' | '?' | '.')))
        .any(|d| wildcard_match(d, &hp.host))
}

/// Built-in UI Content-Security-Policy (+ clickjacking / HTTPS parts).
pub fn content_security_policy(alt_ui: bool, clickjacking: bool, https: bool) -> String {
    let mut csp = String::new();
    if !alt_ui {
        csp.push_str(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; script-src 'self' 'unsafe-inline'; object-src 'none'; form-action 'self'; frame-src 'self' blob:;",
        );
    }
    if clickjacking {
        csp.push_str(" frame-ancestors 'self';");
    }
    if https {
        csp.push_str(" upgrade-insecure-requests;");
    }
    csp
}

/// Headers that may not be overridden by custom headers (framing, cookies,
/// hop-by-hop): letting them through would allow response splitting or
/// session fixation through configuration.
const RESERVED_HEADERS: &[&str] = &[
    "connection",
    "content-encoding",
    "content-length",
    "host",
    "keep-alive",
    "set-cookie",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Parses "Name: value" lines. Returns the headers and warnings for lines
/// that were skipped.
pub fn parse_custom_headers(text: &str) -> (Vec<(HeaderName, HeaderValue)>, Vec<String>) {
    let mut headers = Vec::new();
    let mut warnings = Vec::new();
    for line in text.trim().split('\n').map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            warnings.push(format!("Missing ':' separator in WebUI custom HTTP header: \"{line}\""));
            continue;
        };
        let (name, value) = (name.trim(), value.trim());
        let parsed = (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value));
        match parsed {
            (Ok(n), Ok(v)) if !RESERVED_HEADERS.contains(&n.as_str()) => headers.push((n, v)),
            _ => warnings.push(format!("Invalid WebUI custom HTTP header: \"{line}\"")),
        }
    }
    (headers, warnings)
}

/// Prebuilt headers appended to every response.
pub fn prebuilt_headers(
    alt_ui: bool,
    clickjacking: bool,
    https: bool,
    custom: Vec<(HeaderName, HeaderValue)>,
) -> Vec<(HeaderName, HeaderValue)> {
    let mut h = vec![
        (HeaderName::from_static("x-xss-protection"), HeaderValue::from_static("1; mode=block")),
        (HeaderName::from_static("x-content-type-options"), HeaderValue::from_static("nosniff")),
    ];
    if !alt_ui {
        h.push((HeaderName::from_static("cross-origin-opener-policy"), HeaderValue::from_static("same-origin")));
        h.push((HeaderName::from_static("referrer-policy"), HeaderValue::from_static("same-origin")));
    }
    if clickjacking {
        h.push((HeaderName::from_static("x-frame-options"), HeaderValue::from_static("SAMEORIGIN")));
    }
    let csp = content_security_policy(alt_ui, clickjacking, https);
    if !csp.is_empty()
        && let Ok(v) = HeaderValue::from_str(&csp)
    {
        h.push((HeaderName::from_static("content-security-policy"), v));
    }
    h.extend(custom);
    h
}

/// Makes an untrusted value safe for a single log line.
pub fn sanitize_for_log(s: &str) -> String {
    let mut out: String = s.chars().take(256).map(|c| if c.is_control() { '?' } else { c }).collect();
    if s.chars().count() > 256 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn subnets(list: &[&str]) -> Vec<Subnet> {
        list.iter().map(|s| Subnet::parse(s).unwrap()).collect()
    }

    #[test]
    fn authority_parsing() {
        assert_eq!(parse_authority("localhost:8080"), Some(HostPort { host: "localhost".into(), port: Some(8080) }));
        assert_eq!(parse_authority("http://User@Example.COM/x?y"), Some(HostPort { host: "example.com".into(), port: None }));
        assert_eq!(parse_authority("https://[::1]:9/"), Some(HostPort { host: "::1".into(), port: Some(9) }));
        assert_eq!(parse_authority("host:"), Some(HostPort { host: "host".into(), port: None }));
        assert_eq!(parse_authority("host:99999"), None);
        assert_eq!(parse_authority("::1"), None);
        assert_eq!(parse_authority("[zz]:80"), None);
        assert_eq!(parse_authority("a b:80"), None);
        assert_eq!(parse_authority("null"), Some(HostPort { host: "null".into(), port: None }));
    }

    #[test]
    fn csrf() {
        use CsrfVerdict::*;
        assert_eq!(check_csrf("127.0.0.1:8080", None, None), SameSite);
        assert_eq!(check_csrf("127.0.0.1:8080", Some("http://127.0.0.1:8080"), None), SameSite);
        // Scheme is not compared.
        assert_eq!(check_csrf("127.0.0.1:8080", Some("https://127.0.0.1:8080"), None), SameSite);
        assert_eq!(check_csrf("127.0.0.1:8080", Some("http://evil.com"), None), OriginMismatch);
        assert_eq!(check_csrf("127.0.0.1:8080", Some("http://127.0.0.1:8081"), None), OriginMismatch);
        assert_eq!(check_csrf("127.0.0.1:8080", Some("null"), None), OriginMismatch);
        // Origin wins over Referer.
        assert_eq!(check_csrf("h:1", Some("http://h:1"), Some("http://evil:1/")), SameSite);
        assert_eq!(check_csrf("h:1", None, Some("http://h:1/index.html")), SameSite);
        assert_eq!(check_csrf("h:1", None, Some("http://evil:1/index.html")), RefererMismatch);
        assert_eq!(check_csrf("Example.com", Some("http://EXAMPLE.com"), None), SameSite);
    }

    #[test]
    fn host_header() {
        use HostVerdict::*;
        let local = sa("127.0.0.1:8080");
        let star = vec!["*".to_string()];
        let none: Vec<String> = vec![];
        assert_eq!(validate_host("127.0.0.1:8080", local, &none), Valid);
        assert_eq!(validate_host("127.0.0.1", local, &none), Valid);
        assert_eq!(validate_host("127.0.0.1:9999", local, &star), PortMismatch);
        assert_eq!(validate_host("evil.com:8080", local, &none), Invalid);
        assert_eq!(validate_host("evil.com:8080", local, &star), Valid);
        let doms = vec!["*.example.com".to_string(), "qbt.lan".to_string()];
        assert_eq!(validate_host("a.example.com:8080", local, &doms), Valid);
        assert_eq!(validate_host("QBT.LAN", local, &doms), Valid);
        // Anchored matching: no substring tricks.
        assert_eq!(validate_host("qbt.lan.evil.com", local, &doms), Invalid);
        assert_eq!(validate_host("example.com.evil.net", local, &doms), Invalid);
        // IPv4 client on a dual-stack socket.
        assert_eq!(validate_host("127.0.0.1:8080", sa("[::ffff:127.0.0.1]:8080"), &none), Valid);
        assert_eq!(validate_host("[::1]:8080", sa("[::1]:8080"), &none), Valid);
        assert_eq!(validate_host("", local, &none), Invalid);
        assert_eq!(validate_host("", local, &star), Valid);
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "abbc"));
        assert!(wildcard_match("*.Example.*", "www.example.org"));
        assert!(!wildcard_match("example.com", "www.example.com"));
        assert!(wildcard_match("**a**", "bab"));
    }

    #[test]
    fn client_resolution() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let trusted = subnets(&["10.0.0.1", "10.0.0.2"]);
        // No proxy support: always the socket peer.
        assert_eq!(resolve_client(peer, Some("8.8.8.8"), None), peer);
        // Untrusted peer: header ignored.
        assert_eq!(resolve_client("1.1.1.1".parse().unwrap(), Some("8.8.8.8"), Some(&trusted)), "1.1.1.1".parse::<IpAddr>().unwrap());
        // Trusted peer: rightmost untrusted entry.
        assert_eq!(resolve_client(peer, Some("8.8.8.8"), Some(&trusted)), "8.8.8.8".parse::<IpAddr>().unwrap());
        // Spoofed prefix entries are ignored.
        assert_eq!(
            resolve_client(peer, Some("127.0.0.1, 9.9.9.9, 192.168.1.5"), Some(&trusted)),
            "192.168.1.5".parse::<IpAddr>().unwrap()
        );
        // Chained trusted proxies are skipped.
        assert_eq!(resolve_client(peer, Some("5.5.5.5, 10.0.0.2"), Some(&trusted)), "5.5.5.5".parse::<IpAddr>().unwrap());
        // Garbage stops the walk.
        assert_eq!(resolve_client(peer, Some("5.5.5.5, garbage"), Some(&trusted)), peer);
        assert_eq!(resolve_client(peer, Some("[2001:db8::1]:443"), Some(&trusted)), "2001:db8::1".parse::<IpAddr>().unwrap());
        assert_eq!(resolve_client(peer, None, Some(&trusted)), peer);
        // IPv4-mapped peers are canonicalised.
        assert_eq!(resolve_client("::ffff:10.0.0.1".parse().unwrap(), Some("8.8.4.4"), Some(&trusted)), "8.8.4.4".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn auth_bypass_rules() {
        let wl = subnets(&["192.168.0.0/16"]);
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let lo6: IpAddr = "::1".parse().unwrap();
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let lan: IpAddr = "192.168.1.9".parse().unwrap();
        let wan: IpAddr = "8.8.8.8".parse().unwrap();
        let strict = BypassPolicy { local_auth_required: true, whitelist_enabled: false, whitelist: &wl, proxied: false };
        assert!(is_auth_needed(&strict, lo, lo));
        let local = BypassPolicy { local_auth_required: false, whitelist_enabled: false, whitelist: &wl, proxied: false };
        assert!(!is_auth_needed(&local, lo, lo));
        assert!(!is_auth_needed(&local, lo6, lo6));
        assert!(!is_auth_needed(&local, mapped, mapped));
        // A forwarded loopback address from a remote peer never bypasses.
        assert!(is_auth_needed(&local, wan, lo));
        // A local proxy forwarding a remote client does not bypass either.
        assert!(is_auth_needed(&local, lo, wan));
        let white = BypassPolicy { local_auth_required: true, whitelist_enabled: true, whitelist: &wl, proxied: false };
        assert!(!is_auth_needed(&white, lan, lan));
        assert!(!is_auth_needed(&white, "::ffff:192.168.3.3".parse().unwrap(), "::ffff:192.168.3.3".parse().unwrap()));
        assert!(is_auth_needed(&white, wan, wan));
        let disabled = BypassPolicy { local_auth_required: true, whitelist_enabled: false, whitelist: &wl, proxied: false };
        assert!(is_auth_needed(&disabled, lan, lan));
        // A local reverse proxy (socket peer loopback, no or loopback
        // X-Forwarded-For) never turns its clients into "localhost".
        let proxied = BypassPolicy { local_auth_required: false, whitelist_enabled: false, whitelist: &wl, proxied: true };
        assert!(is_auth_needed(&proxied, lo, lo));
        assert!(is_auth_needed(&proxied, lo6, lo6));
        // The whitelist still applies to the resolved client.
        let proxied_white = BypassPolicy { local_auth_required: true, whitelist_enabled: true, whitelist: &wl, proxied: true };
        assert!(!is_auth_needed(&proxied_white, lo, lan));
    }

    #[test]
    fn bypass_requires_unrebindable_host() {
        let star = vec!["*".to_string()];
        assert!(host_allows_bypass("127.0.0.1:8080", &star));
        assert!(host_allows_bypass("[::1]:8080", &star));
        assert!(host_allows_bypass("192.168.1.10:8080", &star));
        assert!(host_allows_bypass("LOCALHOST:8080", &star));
        // A rebound attacker host name never gets the bypass through `*`.
        assert!(!host_allows_bypass("evil.example:8080", &star));
        assert!(!host_allows_bypass("localhost.evil.example", &star));
        assert!(!host_allows_bypass("evil.example", &["*.*".to_string(), "?*".to_string()]));
        assert!(!host_allows_bypass("", &star));
        assert!(!host_allows_bypass("a b", &star));
        // Explicitly configured domains are trusted.
        let doms = vec!["*".to_string(), "qbt.lan".to_string(), "*.home.arpa".to_string()];
        assert!(host_allows_bypass("qbt.lan:8080", &doms));
        assert!(host_allows_bypass("nas.home.arpa", &doms));
        assert!(!host_allows_bypass("qbt.lan.evil.example", &doms));
    }

    #[test]
    fn headers() {
        let csp = content_security_policy(false, true, true);
        assert!(csp.starts_with("default-src 'self';"));
        assert!(csp.ends_with(" frame-ancestors 'self'; upgrade-insecure-requests;"));
        assert_eq!(content_security_policy(true, false, false), "");
        assert_eq!(content_security_policy(true, true, false), " frame-ancestors 'self';");

        let (h, w) = parse_custom_headers("X-A: 1\nbad line\nX-B:\nSet-Cookie: x=1\nBad Name: v\r\n  \n");
        let names: Vec<&str> = h.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["x-a", "x-b"]);
        assert_eq!(w.len(), 3);
        let all = prebuilt_headers(false, true, false, h);
        let names: Vec<&str> = all.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "x-xss-protection",
                "x-content-type-options",
                "cross-origin-opener-policy",
                "referrer-policy",
                "x-frame-options",
                "content-security-policy",
                "x-a",
                "x-b"
            ]
        );
        let alt = prebuilt_headers(true, false, false, vec![]);
        assert_eq!(alt.len(), 2);
    }

    #[test]
    fn log_sanitizing() {
        assert_eq!(sanitize_for_log("a\r\nb"), "a??b");
        assert_eq!(sanitize_for_log(&"x".repeat(300)).chars().count(), 257);
    }
}
