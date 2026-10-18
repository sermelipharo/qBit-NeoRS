//! Network policy for tracker requests: URL validation, passkey redaction,
//! SSRF mitigation and the IP filter hook.
//!
//! SSRF mitigation (libtorrent `ssrf_mitigation`, plus a stricter rule for
//! private ranges):
//! * an HTTP tracker that resolves to a loopback (or unspecified) address is
//!   only contacted when its path starts with `/announce` (`/scrape` for
//!   scrapes derived from such an URL), so a request to localhost always looks
//!   like a BitTorrent announce;
//! * a tracker whose URL carries its own query string and resolves to a
//!   loopback, private, link-local, unique-local or CGNAT address is refused
//!   unless the tracker is trusted (the torrent came from a local source or the
//!   user added the tracker);
//! * redirects are limited to [`MAX_REDIRECTS`] hops, http/https only, and
//!   never to local addresses;
//! * host names are resolved by [`GuardedResolver`], which drops local
//!   addresses unless the host was vetted for this purpose by
//!   [`NetPolicy::filter_targets`], so DNS rebinding between the check and the
//!   connection cannot reach a local service.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use url::{Host, Url};

use crate::error::TrackerError;

/// Maximum length of a tracker URL.
pub const MAX_URL_LEN: usize = 2048;
/// Maximum number of HTTP redirects followed for one request.
pub const MAX_REDIRECTS: usize = 5;

const LOCAL_HOST_TTL: Duration = Duration::from_secs(600);
const MAX_LOCAL_HOSTS: usize = 256;

/// Predicate returning `true` when an address is blocked by the IP filter.
pub type IpFilterFn = Arc<dyn Fn(IpAddr) -> bool + Send + Sync>;

/// What a request is for; scrapes may use a `/scrape` path on loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Announce,
    Scrape,
}

/// Parse and validate a tracker URL string (trimmed). Unsupported schemes are
/// accepted (they are listed but never contacted).
pub fn parse_tracker_url(s: &str) -> Option<Url> {
    let s = s.trim();
    if s.is_empty() || s.len() > MAX_URL_LEN {
        return None;
    }
    let url = Url::parse(s).ok()?;
    match url.host() {
        Some(Host::Domain("")) => None,
        Some(_) => Some(url),
        None => None,
    }
}

/// Schemes we announce to.
pub fn is_supported_scheme(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "udp")
}

/// A loggable form of a tracker URL: user info dropped, query values and
/// passkey-like path segments masked.
pub fn redact_url(url: &Url) -> String {
    let mut out = format!("{}://", url.scheme());
    match url.host() {
        Some(Host::Ipv6(ip)) => out.push_str(&format!("[{ip}]")),
        Some(h) => out.push_str(&h.to_string()),
        None => {}
    }
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    let path = url
        .path()
        .split('/')
        .map(|seg| {
            if seg.len() >= 16 && seg.bytes().any(|b| b.is_ascii_digit()) {
                "***"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    out.push_str(&path);
    if let Some(q) = url.query().filter(|q| !q.is_empty()) {
        out.push('?');
        let masked = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, _)) => format!("{k}=***"),
                None => "***".to_owned(),
            })
            .collect::<Vec<_>>()
            .join("&");
        out.push_str(&masked);
    }
    out
}

/// Whether an address is in a range that must not be reachable by untrusted
/// URLs: loopback, unspecified, private (RFC 1918), link-local, CGNAT
/// (RFC 6598), unique-local IPv6 and IPv4-mapped forms of those.
pub fn is_local_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                // Deprecated site-local.
                || (s[0] & 0xffc0) == 0xfec0
                // IPv4-compatible (::a.b.c.d) and NAT64 (64:ff9b::/96) forms
                // of local IPv4 addresses.
                || (s[..6] == [0; 6] && is_local_ip(IpAddr::V4(embedded_v4(&s))))
                || (s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] && is_local_ip(IpAddr::V4(embedded_v4(&s))))
        }
    }
}

fn embedded_v4(s: &[u16; 8]) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8)
}

/// Addresses that can never be a tracker: multicast and broadcast.
fn is_never_a_tracker(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_multicast() || ip.is_broadcast(),
        IpAddr::V6(ip) => ip.is_multicast(),
    }
}

fn is_loopback_like(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    ip.is_loopback() || ip.is_unspecified()
}

fn is_localhost_name(name: &str) -> bool {
    let name = name.trim_end_matches('.');
    name.eq_ignore_ascii_case("localhost")
        || name
            .get(name.len().saturating_sub(10)..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(".localhost"))
}

/// Shared, runtime-mutable network policy.
pub struct NetPolicy {
    ssrf_mitigation: AtomicBool,
    ip_filter: RwLock<Option<IpFilterFn>>,
    // Host names vetted by `filter_targets` to resolve to local addresses,
    // with the exact local addresses that were vetted. Only those may be
    // connected to: vetting a host for one LAN address must not let a later
    // resolution (DNS rebinding, or a second A record that was rejected,
    // e.g. 127.0.0.1 next to 10.x) through.
    local_hosts: Mutex<HashMap<String, (Instant, HashSet<IpAddr>)>>,
}

impl std::fmt::Debug for NetPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetPolicy")
            .field("ssrf_mitigation", &self.ssrf_mitigation())
            .field("ip_filter", &self.ip_filter.read().is_some())
            .finish()
    }
}

impl NetPolicy {
    pub fn new(ssrf_mitigation: bool) -> Self {
        Self {
            ssrf_mitigation: AtomicBool::new(ssrf_mitigation),
            ip_filter: RwLock::new(None),
            local_hosts: Mutex::new(HashMap::new()),
        }
    }

    pub fn ssrf_mitigation(&self) -> bool {
        self.ssrf_mitigation.load(Ordering::Relaxed)
    }

    pub fn set_ssrf_mitigation(&self, value: bool) {
        self.ssrf_mitigation.store(value, Ordering::Relaxed);
    }

    /// Set (or clear) the IP filter applied to tracker addresses.
    pub fn set_ip_filter(&self, filter: Option<IpFilterFn>) {
        *self.ip_filter.write() = filter;
    }

    pub fn is_blocked_by_ip_filter(&self, ip: IpAddr) -> bool {
        match &*self.ip_filter.read() {
            Some(f) => f(ip.to_canonical()),
            None => false,
        }
    }

    fn allow_local_host(&self, host: &str, ips: impl IntoIterator<Item = IpAddr>) {
        let mut g = self.local_hosts.lock();
        let now = Instant::now();
        if g.len() >= MAX_LOCAL_HOSTS {
            g.retain(|_, (t, _)| now.duration_since(*t) < LOCAL_HOST_TTL);
            if g.len() >= MAX_LOCAL_HOSTS {
                g.clear();
            }
        }
        let e = g
            .entry(host.to_ascii_lowercase())
            .or_insert_with(|| (now, HashSet::new()));
        if now.duration_since(e.0) >= LOCAL_HOST_TTL {
            e.1.clear();
        }
        e.0 = now;
        // Bounded: a host has only a handful of addresses.
        if e.1.len() < 64 {
            e.1.extend(ips.into_iter().map(|ip| ip.to_canonical()));
        }
    }

    fn is_local_host_allowed(&self, host: &str) -> bool {
        let g = self.local_hosts.lock();
        g.get(&host.to_ascii_lowercase())
            .is_some_and(|(t, _)| t.elapsed() < LOCAL_HOST_TTL)
    }

    /// Whether local address `ip` of `host` was vetted by `filter_targets`.
    fn is_local_addr_allowed(&self, host: &str, ip: IpAddr) -> bool {
        let g = self.local_hosts.lock();
        g.get(&host.to_ascii_lowercase())
            .is_some_and(|(t, ips)| t.elapsed() < LOCAL_HOST_TTL && ips.contains(&ip.to_canonical()))
    }

    /// Apply SSRF mitigation and the IP filter to the resolved addresses of
    /// tracker `url` (the URL as configured, before announce parameters are
    /// appended). Returns the addresses that may be contacted.
    pub fn filter_targets(
        &self,
        url: &Url,
        trusted: bool,
        purpose: Purpose,
        addrs: Vec<SocketAddr>,
    ) -> Result<Vec<SocketAddr>, TrackerError> {
        let ssrf = self.ssrf_mitigation();
        let path = url.path();
        // The path rule only makes sense for HTTP: a UDP tracker request is a
        // binary BitTorrent message whatever the URL path (libtorrent applies
        // it to HTTP trackers only).
        let loopback_path_ok = url.scheme() == "udp"
            || path.starts_with("/announce")
            || (purpose == Purpose::Scrape && path.starts_with("/scrape"));
        let has_query = url.query().is_some_and(|q| !q.is_empty());

        let mut ssrf_blocked = false;
        let mut filter_blocked = false;
        let allowed: Vec<SocketAddr> = addrs
            .into_iter()
            .map(|a| SocketAddr::new(a.ip().to_canonical(), a.port()))
            .filter(|a| {
                let ip = a.ip();
                // Never a tracker: multicast/broadcast/unspecified targets.
                if ssrf && is_never_a_tracker(ip) {
                    ssrf_blocked = true;
                    return false;
                }
                if ssrf
                    && ((is_loopback_like(ip) && !loopback_path_ok)
                        || (is_local_ip(ip) && has_query && !trusted))
                {
                    ssrf_blocked = true;
                    return false;
                }
                if self.is_blocked_by_ip_filter(ip) {
                    filter_blocked = true;
                    return false;
                }
                true
            })
            .collect();

        if allowed.is_empty() {
            return Err(if filter_blocked && !ssrf_blocked {
                TrackerError::other("tracker address is blocked by the IP filter")
            } else if ssrf_blocked {
                TrackerError::other("blocked by SSRF mitigation")
            } else {
                TrackerError::other("tracker host has no addresses")
            });
        }
        let local: Vec<IpAddr> = allowed
            .iter()
            .map(|a| a.ip())
            .filter(|ip| is_local_ip(*ip))
            .collect();
        if !local.is_empty()
            && let Some(Host::Domain(d)) = url.host()
        {
            self.allow_local_host(d, local);
        }
        Ok(allowed)
    }
}

/// DNS resolver for the tracker HTTP client: drops local addresses of hosts
/// that were not vetted, and addresses blocked by the IP filter.
pub(crate) struct GuardedResolver {
    pub(crate) policy: Arc<NetPolicy>,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.policy.clone();
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let ssrf = policy.ssrf_mitigation();
            let filtered: Vec<SocketAddr> = addrs
                .filter(|a| {
                    let ip = a.ip();
                    let local_ok = !ssrf
                        || !(is_local_ip(ip) || is_never_a_tracker(ip))
                        || policy.is_local_addr_allowed(&host, ip);
                    local_ok && !policy.is_blocked_by_ip_filter(ip)
                })
                .collect();
            if filtered.is_empty() {
                return Err("all tracker addresses are blocked by policy".into());
            }
            Ok(Box::new(filtered.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Bounded, policy-checked redirect following.
pub(crate) fn redirect_policy(policy: Arc<NetPolicy>) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let url = attempt.url();
        if !matches!(url.scheme(), "http" | "https") {
            return attempt.error("redirect to an unsupported scheme");
        }
        let ip = match url.host() {
            Some(Host::Ipv4(ip)) => Some(IpAddr::V4(ip)),
            Some(Host::Ipv6(ip)) => Some(IpAddr::V6(ip)),
            Some(Host::Domain(d)) => {
                if policy.ssrf_mitigation()
                    && (is_localhost_name(d) || policy.is_local_host_allowed(d))
                {
                    return attempt.error("redirect blocked by SSRF mitigation");
                }
                None
            }
            None => return attempt.error("redirect without a host"),
        };
        if let Some(ip) = ip {
            if policy.ssrf_mitigation() && is_local_ip(ip) {
                return attempt.error("redirect blocked by SSRF mitigation");
            }
            if policy.is_blocked_by_ip_filter(ip) {
                return attempt.error("redirect blocked by the IP filter");
            }
        }
        attempt.follow()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addrs(s: &[&str]) -> Vec<SocketAddr> {
        s.iter().map(|a| a.parse().unwrap()).collect()
    }

    #[test]
    fn url_parsing() {
        assert!(parse_tracker_url(" http://t.example/announce \n").is_some());
        assert!(parse_tracker_url("udp://t.example:1337").is_some());
        assert!(parse_tracker_url("wss://t.example").is_some());
        assert!(parse_tracker_url("").is_none());
        assert!(parse_tracker_url("not a url").is_none());
        assert!(parse_tracker_url("mailto:foo@example.com").is_none());
        assert!(parse_tracker_url(&format!("http://t.example/{}", "a".repeat(3000))).is_none());
        assert!(is_supported_scheme(
            &parse_tracker_url("https://x/announce").unwrap()
        ));
        assert!(!is_supported_scheme(
            &parse_tracker_url("wss://x/announce").unwrap()
        ));
    }

    #[test]
    fn redaction() {
        let u = Url::parse(
            "https://user:pw@t.example:8443/a/0123456789abcdef0123/announce?passkey=SECRET&x",
        )
        .unwrap();
        let r = redact_url(&u);
        assert_eq!(r, "https://t.example:8443/a/***/announce?passkey=***&***");
        assert!(!r.contains("SECRET"));
        let u = Url::parse("udp://[::1]:6969/announce").unwrap();
        assert_eq!(redact_url(&u), "udp://[::1]:6969/announce");
    }

    #[test]
    fn local_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.0.1",
        ] {
            assert!(is_local_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "100.128.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_local_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["fec0::1", "::127.0.0.1", "::10.0.0.1", "64:ff9b::192.168.0.1", "64:ff9b::7f00:1"] {
            assert!(is_local_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(!is_local_ip("64:ff9b::8.8.8.8".parse().unwrap()));
        assert!(is_localhost_name("localhost"));
        assert!(is_localhost_name("LOCALHOST."));
        assert!(is_localhost_name("foo.localhost"));
        assert!(!is_localhost_name("notlocalhost.com"));
    }

    #[test]
    fn ssrf_loopback_requires_announce_path() {
        let p = NetPolicy::new(true);
        let u = Url::parse("http://127.0.0.1:8080/announce").unwrap();
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, addrs(&["127.0.0.1:8080"]))
                .is_ok()
        );
        let u = Url::parse("http://127.0.0.1:8080/admin").unwrap();
        assert!(
            p.filter_targets(&u, true, Purpose::Announce, addrs(&["127.0.0.1:8080"]))
                .is_err()
        );
        let u = Url::parse("http://127.0.0.1:8080/scrape").unwrap();
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, addrs(&["127.0.0.1:8080"]))
                .is_err()
        );
        assert!(
            p.filter_targets(&u, false, Purpose::Scrape, addrs(&["127.0.0.1:8080"]))
                .is_ok()
        );
        // UDP trackers have no meaningful path.
        let u = Url::parse("udp://127.0.0.1:6969").unwrap();
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, addrs(&["127.0.0.1:6969"]))
                .is_ok()
        );
        let u = Url::parse("http://0.0.0.0:8080/x").unwrap();
        assert!(
            p.filter_targets(&u, true, Purpose::Announce, addrs(&["0.0.0.0:8080"]))
                .is_err()
        );
    }

    #[test]
    fn ssrf_private_with_query_requires_trust() {
        let p = NetPolicy::new(true);
        let u = Url::parse("http://router.lan/announce?cmd=reboot").unwrap();
        let a = addrs(&["192.168.1.1:80"]);
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, a.clone())
                .is_err()
        );
        assert!(
            p.filter_targets(&u, true, Purpose::Announce, a.clone())
                .is_ok()
        );
        // Mixed results keep the public addresses only.
        let mixed = addrs(&["192.168.1.1:80", "8.8.8.8:80"]);
        assert_eq!(
            p.filter_targets(&u, false, Purpose::Announce, mixed)
                .unwrap(),
            addrs(&["8.8.8.8:80"])
        );
        // No query string: private trackers on the LAN are fine.
        let u = Url::parse("http://tracker.lan/announce").unwrap();
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, a.clone())
                .is_ok()
        );
        assert!(p.is_local_host_allowed("TRACKER.lan"));
        assert!(p.is_local_addr_allowed("TRACKER.lan", "192.168.1.1".parse().unwrap()));
        assert!(!p.is_local_addr_allowed("tracker.lan", "127.0.0.1".parse().unwrap()));
        assert!(!p.is_local_addr_allowed("tracker.lan", "192.168.1.2".parse().unwrap()));
        // Disabled mitigation allows everything.
        let p = NetPolicy::new(false);
        let u = Url::parse("http://127.0.0.1/admin?x=1").unwrap();
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, addrs(&["127.0.0.1:80"]))
                .is_ok()
        );
    }

    #[test]
    fn vetting_covers_only_the_checked_local_addresses() {
        let p = NetPolicy::new(true);
        // evil.example resolves to 10.9.9.9 and 127.0.0.1; the path rule
        // drops loopback, so only 10.9.9.9 may later be connected to.
        let u = Url::parse("http://evil.example:8080/admin").unwrap();
        let a = addrs(&["10.9.9.9:8080", "127.0.0.1:8080"]);
        assert_eq!(
            p.filter_targets(&u, false, Purpose::Announce, a).unwrap(),
            addrs(&["10.9.9.9:8080"])
        );
        assert!(p.is_local_addr_allowed("evil.example", "10.9.9.9".parse().unwrap()));
        assert!(!p.is_local_addr_allowed("evil.example", "127.0.0.1".parse().unwrap()));
        assert!(!p.is_local_addr_allowed("evil.example", "192.168.0.1".parse().unwrap()));
        // Multicast / broadcast are never trackers (UDP skips the path rule).
        let udp = Url::parse("udp://239.255.255.250:1900").unwrap();
        assert!(
            p.filter_targets(&udp, true, Purpose::Announce, addrs(&["239.255.255.250:1900"]))
                .is_err()
        );
        assert!(
            p.filter_targets(&udp, true, Purpose::Announce, addrs(&["255.255.255.255:1900"]))
                .is_err()
        );
    }

    #[test]
    fn ip_filter() {
        let p = NetPolicy::new(true);
        p.set_ip_filter(Some(Arc::new(|ip: IpAddr| {
            ip == "8.8.8.8".parse::<IpAddr>().unwrap()
        })));
        let u = Url::parse("http://t.example/announce").unwrap();
        let e = p
            .filter_targets(&u, false, Purpose::Announce, addrs(&["8.8.8.8:80"]))
            .unwrap_err();
        assert!(e.message.contains("IP filter"));
        assert_eq!(
            p.filter_targets(
                &u,
                false,
                Purpose::Announce,
                addrs(&["8.8.8.8:80", "1.1.1.1:80"])
            )
            .unwrap(),
            addrs(&["1.1.1.1:80"])
        );
        p.set_ip_filter(None);
        assert!(
            p.filter_targets(&u, false, Purpose::Announce, addrs(&["8.8.8.8:80"]))
                .is_ok()
        );
    }
}
