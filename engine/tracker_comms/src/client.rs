//! One announce or scrape to one tracker, over HTTP(S) or UDP, with the
//! session's network policy applied.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use librqbit_core::hash_id::Id20;
use parking_lot::{Mutex, RwLock};
use serde_derive::Serialize;
use tokio::sync::Notify;
use url::{Host, Url};

use crate::error::TrackerError;
use crate::policy::{GuardedResolver, NetPolicy, Purpose, redirect_policy};
use crate::response::{
    AnnounceResponse, ScrapeData, parse_announce_response, parse_scrape_response,
};
use crate::tracker_comms_http::{
    AnnounceRequest, http_get, scrape_querystring, scrape_url, url_with_params,
};
use crate::tracker_comms_udp::{AnnounceFields, UdpTrackerClient};

/// The address family an announce went over. HTTP announces are reported with
/// the family of the connection that answered (`Any` if unknown).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum AddressFamily {
    Any,
    V4,
    V6,
}

impl AddressFamily {
    pub fn of(addr: &SocketAddr) -> Self {
        if addr.ip().to_canonical().is_ipv4() {
            AddressFamily::V4
        } else {
            AddressFamily::V6
        }
    }
}

#[derive(Debug, Clone)]
pub struct EndpointOutcome {
    pub family: AddressFamily,
    pub result: Result<AnnounceResponse, TrackerError>,
}

/// Results of announcing to one tracker (one per address family for UDP).
#[derive(Debug, Clone)]
pub struct AnnounceOutcome {
    pub endpoints: Vec<EndpointOutcome>,
}

impl AnnounceOutcome {
    pub fn single(family: AddressFamily, result: Result<AnnounceResponse, TrackerError>) -> Self {
        Self {
            endpoints: vec![EndpointOutcome { family, result }],
        }
    }

    pub fn is_success(&self) -> bool {
        self.endpoints.iter().any(|e| e.result.is_ok())
    }

    pub fn peers(&self) -> impl Iterator<Item = SocketAddr> + '_ {
        self.endpoints
            .iter()
            .filter_map(|e| e.result.as_ref().ok())
            .flat_map(|r| r.peers.iter().copied())
    }
}

/// Per-call options.
#[derive(Clone, Debug)]
pub struct CallOptions {
    /// `User-Agent` header for HTTP trackers (`None` = no header).
    pub user_agent: Option<String>,
    pub timeout: Duration,
    /// Only use IPv4 tracker addresses.
    pub ipv4_only: bool,
}

/// Options of the HTTP client; changing them rebuilds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpClientOptions {
    pub validate_https_certificates: bool,
    /// Connect over IPv4 only.
    pub ipv4_only: bool,
    /// A proxy is applied by the factory: host names are then resolved by the
    /// proxy, so only literal addresses can be checked before connecting.
    pub proxied: bool,
}

impl Default for HttpClientOptions {
    fn default() -> Self {
        Self {
            validate_https_certificates: true,
            ipv4_only: false,
            proxied: false,
        }
    }
}

/// Finishes a pre-configured builder (resolver, redirect policy and timeouts
/// are already set): applies TLS options, proxy and interface binding. The
/// TLS backend lives in the embedding crate, hence the factory.
pub type HttpClientFactory = Arc<
    dyn Fn(reqwest::ClientBuilder, &HttpClientOptions) -> anyhow::Result<reqwest::Client>
        + Send
        + Sync,
>;

/// Resizable concurrency limit (0 = unlimited).
struct Limiter {
    state: Mutex<(usize, usize)>,
    notify: Notify,
}

struct LimiterGuard<'a>(&'a Limiter);

impl Drop for LimiterGuard<'_> {
    fn drop(&mut self) {
        self.0.state.lock().0 -= 1;
        self.0.notify.notify_one();
    }
}

impl Limiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new((0, limit)),
            notify: Notify::new(),
        }
    }

    fn set_limit(&self, limit: usize) {
        self.state.lock().1 = limit;
        self.notify.notify_waiters();
    }

    async fn acquire(&self) -> LimiterGuard<'_> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut g = self.state.lock();
                if g.1 == 0 || g.0 < g.1 {
                    g.0 += 1;
                    return LimiterGuard(self);
                }
            }
            notified.await;
        }
    }
}

/// See [`I2pTransport::map_peer`].
pub type I2pPeerMapper = Arc<dyn Fn(&str) -> Option<SocketAddr> + Send + Sync>;

/// How `.i2p` HTTP trackers are reached (set while an I2P session is up).
pub struct I2pTransport {
    /// A client whose proxy tunnels `*.i2p` hosts through the I2P router.
    pub client: reqwest::Client,
    /// Our base64 destination, announced as `ip=<destination>.i2p`.
    pub destination: String,
    /// Maps an I2P peer (base64 destination or `*.i2p` name) to the address
    /// the engine uses for it; `None` drops the peer.
    pub map_peer: I2pPeerMapper,
}

pub(crate) fn is_i2p_host(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|h| h.to_ascii_lowercase().ends_with(".i2p"))
}

pub struct TrackerClient {
    policy: Arc<NetPolicy>,
    http: RwLock<reqwest::Client>,
    http_factory: HttpClientFactory,
    http_options: Mutex<HttpClientOptions>,
    http_limiter: Limiter,
    udp: UdpTrackerClient,
    /// UDP trackers are skipped (e.g. while trackers go through a proxy that
    /// can't relay UDP).
    udp_disabled: std::sync::atomic::AtomicBool,
    i2p: RwLock<Option<Arc<I2pTransport>>>,
}

fn default_factory() -> HttpClientFactory {
    Arc::new(|b, _| Ok(b.build()?))
}

impl TrackerClient {
    pub fn new(
        udp: UdpTrackerClient,
        policy: Arc<NetPolicy>,
        http_factory: Option<HttpClientFactory>,
        http_options: HttpClientOptions,
        max_concurrent_http_announces: usize,
    ) -> anyhow::Result<Self> {
        let http_factory = http_factory.unwrap_or_else(default_factory);
        let http = Self::build_http(&policy, &http_factory, &http_options)?;
        Ok(Self {
            policy,
            http: RwLock::new(http),
            http_factory,
            http_options: Mutex::new(http_options),
            http_limiter: Limiter::new(max_concurrent_http_announces),
            udp,
            udp_disabled: Default::default(),
            i2p: RwLock::new(None),
        })
    }

    fn build_http(
        policy: &Arc<NetPolicy>,
        factory: &HttpClientFactory,
        opts: &HttpClientOptions,
    ) -> anyhow::Result<reqwest::Client> {
        let mut b = reqwest::Client::builder()
            .dns_resolver(GuardedResolver {
                policy: policy.clone(),
            })
            .redirect(redirect_policy(policy.clone()))
            .referer(false)
            .connect_timeout(Duration::from_secs(15))
            .pool_idle_timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(2);
        if opts.ipv4_only {
            b = b.local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        }
        factory(b, opts)
    }

    pub fn policy(&self) -> &Arc<NetPolicy> {
        &self.policy
    }

    pub fn http_options(&self) -> HttpClientOptions {
        self.http_options.lock().clone()
    }

    /// Change HTTP client options (rebuilds the client if they differ).
    pub fn set_http_options(&self, opts: HttpClientOptions) -> anyhow::Result<()> {
        let mut cur = self.http_options.lock();
        if *cur == opts {
            return Ok(());
        }
        let client = Self::build_http(&self.policy, &self.http_factory, &opts)?;
        *self.http.write() = client;
        *cur = opts;
        Ok(())
    }

    /// Rebuild the HTTP client with the current options (e.g. after the
    /// factory's proxy changed).
    pub fn rebuild_http(&self) -> anyhow::Result<()> {
        let cur = self.http_options.lock();
        let client = Self::build_http(&self.policy, &self.http_factory, &cur)?;
        *self.http.write() = client;
        Ok(())
    }

    /// Reach `.i2p` HTTP trackers through I2P (`None`: they are skipped).
    pub fn set_i2p_transport(&self, t: Option<I2pTransport>) {
        *self.i2p.write() = t.map(Arc::new);
    }

    async fn announce_i2p(
        &self,
        url: &Url,
        req: &AnnounceRequest,
        call: &CallOptions,
    ) -> AnnounceOutcome {
        let Some(t) = self.i2p.read().clone() else {
            return AnnounceOutcome::single(
                AddressFamily::Any,
                Err(TrackerError::skipped("I2P is not enabled")),
            );
        };
        let mut req = req.clone();
        req.ip = None;
        let mut query = req.as_querystring();
        query.push_str("&ip=");
        query.push_str(&urlencoding::encode(&format!("{}.i2p", t.destination)));
        let _permit = self.http_limiter.acquire().await;
        let full = url_with_params(url, &query);
        let result = http_get(&t.client, full, call.user_agent.as_deref(), call.timeout)
            .await
            .and_then(|(body, _)| {
                let mut r = parse_announce_response(&body)?;
                // Compact I2P peers are 32-byte hashes, not IPv4 entries.
                r.peers = crate::response::parse_i2p_peers(&body)
                    .iter()
                    .filter_map(|p| (t.map_peer)(p))
                    .collect();
                Ok(r)
            });
        AnnounceOutcome::single(AddressFamily::Any, result)
    }

    /// Skip UDP trackers ("Unreachable") while `disabled`.
    pub fn set_udp_disabled(&self, disabled: bool) {
        self.udp_disabled
            .store(disabled, std::sync::atomic::Ordering::Relaxed);
    }

    fn udp_skipped(&self) -> Option<TrackerError> {
        self.udp_disabled
            .load(std::sync::atomic::Ordering::Relaxed)
            .then(|| TrackerError::skipped("UDP trackers are not used through the proxy"))
    }

    pub fn set_max_concurrent_http_announces(&self, limit: usize) {
        self.http_limiter.set_limit(limit);
    }

    pub fn udp(&self) -> &UdpTrackerClient {
        &self.udp
    }

    async fn resolve(&self, url: &Url, port: u16) -> Result<Vec<SocketAddr>, TrackerError> {
        match url.host() {
            Some(Host::Domain(d)) => tokio::net::lookup_host((d, port))
                .await
                .map(|a| a.collect())
                .map_err(|e| TrackerError::other(format!("DNS lookup failed: {e}"))),
            Some(Host::Ipv4(ip)) => Ok(vec![SocketAddr::new(ip.into(), port)]),
            Some(Host::Ipv6(ip)) => Ok(vec![SocketAddr::new(ip.into(), port)]),
            None => Err(TrackerError::other("tracker URL has no host").permanent()),
        }
    }

    /// Check an HTTP URL against the policy before connecting. Returns whether
    /// the tracker is on a loopback address.
    async fn precheck_http(
        &self,
        url: &Url,
        trusted: bool,
        purpose: Purpose,
    ) -> Result<bool, TrackerError> {
        let port = url
            .port_or_known_default()
            .ok_or_else(|| TrackerError::other("tracker URL has no port").permanent())?;
        let literal = !matches!(url.host(), Some(Host::Domain(_)));
        if self.http_options.lock().proxied && !literal {
            // The proxy resolves the name; only a literal "localhost" can be judged here.
            if let Some(Host::Domain(d)) = url.host()
                && d.eq_ignore_ascii_case("localhost")
            {
                let addrs = vec![SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)];
                self.policy.filter_targets(url, trusted, purpose, addrs)?;
                return Ok(true);
            }
            return Ok(false);
        }
        let addrs = self.resolve(url, port).await?;
        let allowed = self.policy.filter_targets(url, trusted, purpose, addrs)?;
        Ok(allowed.iter().all(|a| a.ip().is_loopback()))
    }

    /// Announce to one tracker. Never fails as a whole: errors are reported
    /// per endpoint.
    pub async fn announce(
        &self,
        url: &Url,
        trusted: bool,
        req: &AnnounceRequest,
        call: &CallOptions,
    ) -> AnnounceOutcome {
        let fut = async {
            match url.scheme() {
                "http" | "https" if is_i2p_host(url) => self.announce_i2p(url, req, call).await,
                "http" | "https" => self.announce_http(url, trusted, req, call).await,
                "udp" if is_i2p_host(url) => AnnounceOutcome::single(
                    AddressFamily::Any,
                    Err(TrackerError::skipped("UDP trackers are not supported over I2P")),
                ),
                "udp" => self.announce_udp(url, trusted, req, call).await,
                _ => AnnounceOutcome::single(
                    AddressFamily::Any,
                    Err(TrackerError::other("unsupported URL protocol").permanent()),
                ),
            }
        };
        match tokio::time::timeout(call.timeout, fut).await {
            Ok(o) => o,
            Err(_) => AnnounceOutcome::single(
                AddressFamily::Any,
                Err(TrackerError::other("tracker timed out")),
            ),
        }
    }

    async fn announce_http(
        &self,
        url: &Url,
        trusted: bool,
        req: &AnnounceRequest,
        call: &CallOptions,
    ) -> AnnounceOutcome {
        let tracker_is_loopback = match self.precheck_http(url, trusted, Purpose::Announce).await {
            Ok(l) => l,
            Err(e) => return AnnounceOutcome::single(AddressFamily::Any, Err(e)),
        };
        let _permit = self.http_limiter.acquire().await;
        let full = url_with_params(url, &req.as_querystring());
        let client = self.http.read().clone();
        match http_get(&client, full, call.user_agent.as_deref(), call.timeout).await {
            Ok((body, remote)) => {
                let family = remote
                    .as_ref()
                    .map(AddressFamily::of)
                    .unwrap_or(AddressFamily::Any);
                let result = parse_announce_response(&body).map(|mut r| {
                    retain_reachable_peers(&mut r, tracker_is_loopback);
                    r
                });
                AnnounceOutcome::single(family, result)
            }
            Err(e) => AnnounceOutcome::single(AddressFamily::Any, Err(e)),
        }
    }

    async fn announce_udp(
        &self,
        url: &Url,
        trusted: bool,
        req: &AnnounceRequest,
        call: &CallOptions,
    ) -> AnnounceOutcome {
        if let Some(e) = self.udp_skipped() {
            return AnnounceOutcome::single(AddressFamily::Any, Err(e));
        }
        let Some(port) = url.port() else {
            return AnnounceOutcome::single(
                AddressFamily::Any,
                Err(TrackerError::other("UDP tracker URL has no port").permanent()),
            );
        };
        let addrs = match self.resolve(url, port).await.and_then(|a| {
            self.policy
                .filter_targets(url, trusted, Purpose::Announce, a)
        }) {
            Ok(a) => a,
            Err(e) => return AnnounceOutcome::single(AddressFamily::Any, Err(e)),
        };
        let v4 = addrs.iter().copied().find(|a| a.is_ipv4());
        let v6 = addrs.iter().copied().find(|a| a.is_ipv6());
        let v6 = if call.ipv4_only { None } else { v6 };
        if v4.is_none() && v6.is_none() {
            return AnnounceOutcome::single(
                AddressFamily::V6,
                Err(TrackerError::skipped("tracker has no IPv4 address")),
            );
        }

        let mut url_data = url.path().as_bytes().to_vec();
        if url_data == b"/" {
            url_data.clear();
        }
        if let Some(q) = url.query() {
            url_data.push(b'?');
            url_data.extend_from_slice(q.as_bytes());
        }
        let fields = AnnounceFields {
            info_hash: req.info_hash,
            peer_id: req.peer_id,
            downloaded: req.downloaded,
            left: req.left,
            uploaded: req.uploaded,
            event: req.event.udp_code(),
            ip: match req.ip {
                Some(IpAddr::V4(ip)) => u32::from(ip),
                _ => 0,
            },
            key: req.key,
            num_want: req
                .num_want
                .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
                .unwrap_or(-1),
            port: req.port,
            url_data,
        };

        let one = |addr: Option<SocketAddr>| {
            let fields = fields.clone();
            async move {
                let addr = addr?;
                let mut result = self.udp.announce(addr, fields).await;
                if let Ok(r) = &mut result {
                    retain_reachable_peers(r, addr.ip().is_loopback());
                }
                Some(EndpointOutcome {
                    family: AddressFamily::of(&addr),
                    result,
                })
            }
        };
        let (r4, r6) = tokio::join!(one(v4), one(v6));
        AnnounceOutcome {
            endpoints: r4.into_iter().chain(r6).collect(),
        }
    }

    /// Scrape one tracker for `info_hash`. Trackers without a scrape URL return
    /// an error of kind [`crate::TrackerErrorKind::Skipped`].
    pub async fn scrape(
        &self,
        url: &Url,
        trusted: bool,
        info_hash: Id20,
        call: &CallOptions,
    ) -> Result<ScrapeData, TrackerError> {
        let fut = async {
            match url.scheme() {
                "http" | "https" if is_i2p_host(url) => {
                    Err(TrackerError::skipped("scrape not supported over I2P"))
                }
                "http" | "https" => {
                    let surl = scrape_url(url)
                        .ok_or_else(|| TrackerError::skipped("scrape not supported"))?;
                    self.precheck_http(&surl, trusted, Purpose::Scrape).await?;
                    let _permit = self.http_limiter.acquire().await;
                    let full = url_with_params(&surl, &scrape_querystring(&info_hash));
                    let client = self.http.read().clone();
                    let (body, _) =
                        http_get(&client, full, call.user_agent.as_deref(), call.timeout).await?;
                    parse_scrape_response(&body, &info_hash)
                }
                "udp" => {
                    if let Some(e) = self.udp_skipped() {
                        return Err(e);
                    }
                    let port = url
                        .port()
                        .ok_or_else(|| TrackerError::skipped("scrape not supported"))?;
                    let addrs = self.resolve(url, port).await?;
                    let addrs = self
                        .policy
                        .filter_targets(url, trusted, Purpose::Scrape, addrs)?;
                    let addr = addrs
                        .iter()
                        .find(|a| a.is_ipv4())
                        .or_else(|| addrs.iter().find(|_| !call.ipv4_only))
                        .copied()
                        .ok_or_else(|| TrackerError::skipped("tracker has no IPv4 address"))?;
                    self.udp.scrape(addr, info_hash).await
                }
                _ => Err(TrackerError::skipped("scrape not supported")),
            }
        };
        tokio::time::timeout(call.timeout, fut)
            .await
            .unwrap_or_else(|_| Err(TrackerError::other("tracker timed out")))
    }
}

/// Drop loopback peers handed out by a non-loopback tracker: a remote tracker
/// must not be able to make us connect to local services.
fn retain_reachable_peers(r: &mut AnnounceResponse, tracker_is_loopback: bool) {
    if !tracker_is_loopback {
        r.peers.retain(|p| !p.ip().to_canonical().is_loopback());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limiter_respects_and_resizes_limit() {
        let l = Arc::new(Limiter::new(1));
        let g1 = l.acquire().await;
        let l2 = l.clone();
        let waiter = tokio::spawn(async move {
            let _g = l2.acquire().await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        drop(g1);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();

        let g1 = l.acquire().await;
        let l2 = l.clone();
        let waiter = tokio::spawn(async move {
            let _g = l2.acquire().await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        l.set_limit(0);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .unwrap()
            .unwrap();
        drop(g1);
    }

    #[test]
    fn loopback_peers_dropped_for_remote_trackers() {
        let mut r = AnnounceResponse {
            peers: vec![
                "127.0.0.1:1".parse().unwrap(),
                "[::1]:2".parse().unwrap(),
                "8.8.8.8:3".parse().unwrap(),
            ],
            ..Default::default()
        };
        let mut local = r.clone();
        retain_reachable_peers(&mut r, false);
        assert_eq!(r.peers, vec!["8.8.8.8:3".parse().unwrap()]);
        retain_reachable_peers(&mut local, true);
        assert_eq!(local.peers.len(), 3);
    }

    mod network {
        use super::super::*;
        use crate::error::TrackerErrorKind;
        use crate::tracker_comms_http::AnnounceEvent;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_util::sync::CancellationToken;

        /// Serves canned responses by path; counts requests.
        async fn http_server() -> (SocketAddr, Arc<AtomicUsize>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let hits = Arc::new(AtomicUsize::new(0));
            let h = hits.clone();
            tokio::spawn(async move {
                loop {
                    let (mut sock, _) = listener.accept().await.unwrap();
                    h.fetch_add(1, Ordering::SeqCst);
                    tokio::spawn(async move {
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 2048];
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            match sock.read(&mut chunk).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            }
                        }
                        let req = String::from_utf8_lossy(&buf).into_owned();
                        let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        let path = path.split('?').next().unwrap_or("/");
                        let (status, extra, body): (&str, String, Vec<u8>) = match path {
                            "/announce" => {
                                let mut b = b"d8:intervali900e5:peers12:".to_vec();
                                b.extend_from_slice(&[127, 0, 0, 1, 0, 1, 8, 8, 8, 8, 0, 2]);
                                b.push(b'e');
                                ("200 OK", String::new(), b)
                            }
                            "/announce.big" => {
                                ("200 OK", String::new(), vec![b'x'; 2 * 1024 * 1024])
                            }
                            "/announce.gone" => ("410 Gone", String::new(), Vec::new()),
                            "/announce.redirect" => (
                                "302 Found",
                                format!("Location: http://{addr}/announce\r\n"),
                                Vec::new(),
                            ),
                            _ => ("404 Not Found", String::new(), Vec::new()),
                        };
                        let head = format!(
                            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(&body).await;
                    });
                }
            });
            (addr, hits)
        }

        async fn client(ssrf: bool) -> TrackerClient {
            let udp = UdpTrackerClient::new(CancellationToken::new(), None)
                .await
                .unwrap();
            TrackerClient::new(
                udp,
                Arc::new(NetPolicy::new(ssrf)),
                None,
                HttpClientOptions::default(),
                4,
            )
            .unwrap()
        }

        fn req() -> AnnounceRequest {
            AnnounceRequest {
                info_hash: Id20::new([1; 20]),
                peer_id: Id20::new([2; 20]),
                port: 6881,
                uploaded: 0,
                downloaded: 0,
                left: 1,
                event: AnnounceEvent::Started,
                key: 1,
                num_want: Some(50),
                ip: None,
                tracker_id: None,
            }
        }

        fn call() -> CallOptions {
            CallOptions {
                user_agent: Some("test/1.0".into()),
                timeout: Duration::from_secs(10),
                ipv4_only: false,
            }
        }

        fn single_err(o: &AnnounceOutcome) -> &TrackerError {
            assert_eq!(o.endpoints.len(), 1);
            o.endpoints[0].result.as_ref().unwrap_err()
        }

        #[tokio::test]
        async fn http_announce_policy() {
            let (addr, hits) = http_server().await;
            let c = client(true).await;
            let url = |p: &str| Url::parse(&format!("http://{addr}{p}")).unwrap();

            // A loopback tracker answering like a tracker: loopback peers kept.
            let o = c.announce(&url("/announce"), false, &req(), &call()).await;
            let r = o.endpoints[0].result.as_ref().unwrap();
            assert_eq!(o.endpoints[0].family, AddressFamily::V4);
            assert_eq!(r.peers.len(), 2);
            assert_eq!(hits.load(Ordering::SeqCst), 1);

            // SSRF: a loopback URL that is not an announce is never requested.
            let o = c.announce(&url("/admin"), true, &req(), &call()).await;
            assert!(single_err(&o).message.contains("SSRF"));
            let o = c
                .announce(&url("/announce?cmd=1"), false, &req(), &call())
                .await;
            assert!(single_err(&o).message.contains("SSRF"));
            assert_eq!(hits.load(Ordering::SeqCst), 1);

            // Redirects to local addresses are refused.
            let o = c
                .announce(&url("/announce.redirect"), false, &req(), &call())
                .await;
            assert!(single_err(&o).message.contains("redirect"), "{o:?}");
            assert_eq!(hits.load(Ordering::SeqCst), 2);

            // Oversized bodies are rejected.
            let o = c
                .announce(&url("/announce.big"), false, &req(), &call())
                .await;
            assert!(single_err(&o).message.contains("too large"));

            // 410 Gone: never retry.
            let o = c
                .announce(&url("/announce.gone"), false, &req(), &call())
                .await;
            assert!(single_err(&o).never_retry);
            assert_eq!(single_err(&o).kind, TrackerErrorKind::Other);

            // Unsupported schemes are never contacted.
            let o = c
                .announce(
                    &Url::parse("wss://t.example/announce").unwrap(),
                    false,
                    &req(),
                    &call(),
                )
                .await;
            assert!(single_err(&o).never_retry);

            // The IP filter applies to trackers.
            c.policy()
                .set_ip_filter(Some(Arc::new(|ip: IpAddr| ip.is_loopback())));
            let before = hits.load(Ordering::SeqCst);
            let o = c.announce(&url("/announce"), false, &req(), &call()).await;
            assert!(single_err(&o).message.contains("IP filter"));
            assert_eq!(hits.load(Ordering::SeqCst), before);
            c.policy().set_ip_filter(None);

            // Without mitigation the redirect is followed.
            let c = client(false).await;
            let o = c
                .announce(&url("/announce.redirect"), false, &req(), &call())
                .await;
            assert!(o.is_success(), "{o:?}");

            // Scrape URL derivation failure is "skipped".
            let e = c
                .scrape(&url("/a"), false, Id20::new([1; 20]), &call())
                .await
                .unwrap_err();
            assert_eq!(e.kind, TrackerErrorKind::Skipped);
        }

        #[tokio::test]
        async fn udp_announce_through_client() {
            let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let addr = server.local_addr().unwrap();
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                loop {
                    let (len, from) = server.recv_from(&mut buf).await.unwrap();
                    let req = &buf[..len];
                    let action = u32::from_be_bytes(req[8..12].try_into().unwrap());
                    let mut resp = Vec::new();
                    resp.extend_from_slice(&action.to_be_bytes());
                    resp.extend_from_slice(&req[12..16]);
                    if action == 0 {
                        resp.extend_from_slice(&7u64.to_be_bytes());
                    } else {
                        // Echo the event as the interval so the test can check it.
                        resp.extend_from_slice(&req[80..84]);
                        resp.extend_from_slice(&1u32.to_be_bytes());
                        resp.extend_from_slice(&2u32.to_be_bytes());
                        resp.extend_from_slice(&[127, 0, 0, 1, 0x1a, 0xe1]);
                    }
                    server.send_to(&resp, from).await.unwrap();
                }
            });
            let c = client(true).await;
            let url = Url::parse(&format!("udp://{addr}/announce")).unwrap();
            let o = c.announce(&url, false, &req(), &call()).await;
            assert_eq!(o.endpoints.len(), 1);
            assert_eq!(o.endpoints[0].family, AddressFamily::V4);
            let r = o.endpoints[0].result.as_ref().unwrap();
            assert_eq!(r.interval, Some(crate::tracker_comms_udp::EVENT_STARTED));
            assert_eq!(r.complete, Some(2));
            assert_eq!(r.incomplete, Some(1));
            assert_eq!(r.peers, vec!["127.0.0.1:6881".parse().unwrap()]);

            let url = Url::parse(&format!("udp://{addr}")).unwrap();
            let o = c.announce(&url, false, &req(), &call()).await;
            assert!(o.is_success());

            // Behind a proxy without UDP relaying UDP trackers are skipped.
            c.set_udp_disabled(true);
            let o = c.announce(&url, false, &req(), &call()).await;
            assert!(!o.is_success());
            let e = o.endpoints[0].result.as_ref().unwrap_err();
            assert_eq!(e.kind, crate::TrackerErrorKind::Skipped);
            c.set_udp_disabled(false);
            assert!(c.announce(&url, false, &req(), &call()).await.is_success());
        }
    }
}
