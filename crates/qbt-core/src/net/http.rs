//! Shared HTTP client for non-BitTorrent traffic (RSS, search plugins,
//! .torrent downloads, tracker lists, GeoIP, DynDNS) — qBittorrent's
//! `Net::DownloadManager` / `DownloadHandlerImpl`.
//!
//! * Proxy: the Connection-page proxy applies per purpose (`proxy_rss`,
//!   `proxy_misc`, `proxy_bittorrent`); HTTP, SOCKS5 and SOCKS4 proxies,
//!   optional authentication, hostname lookup through the proxy (`socks5h` /
//!   `socks4a`). A misconfigured enabled proxy fails the request instead of
//!   silently connecting directly.
//! * TLS certificates are validated (platform verifier) unless
//!   "Ignore SSL errors" (`ignore_ssl_errors`) is set.
//! * Redirects are followed manually: at most [`MAX_REDIRECTS`], only to
//!   http(s); a redirect to a `magnet:` URI ends the request with the URI in
//!   [`HttpResponse::url`] (qBittorrent's "RedirectedToMagnet").
//! * The size limit is enforced while streaming (and up front from
//!   `Content-Length`); gzip bodies are decompressed by the client and the
//!   limit applies to the decompressed bytes (no zip bombs).
//! * Timeouts: connect 30 s, response headers 60 s, 30 s without receiving
//!   data, plus the optional per-request total `timeout_secs`.
//! * A persistent cookie jar (`config/cookies.json`, mode 0600) is used for
//!   every request; see [`super::cookies`].
//! * Clients are built lazily per purpose and rebuilt when the relevant
//!   preferences change.
//!
//! Credentials (proxy passwords, URL user info) never appear in errors or logs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::Url;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE, COOKIE, LOCATION, REFERER, SET_COOKIE, USER_AGENT};

use super::cookies::CookieJar;
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::model::Cookie;
use crate::prefs::{ConnectionPrefs, Preferences, ProxyType};

/// Which proxy profile applies (Connection page: "Use proxy for RSS purposes",
/// "Use proxy for general purposes", "Use proxy for BitTorrent purposes").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HttpPurpose {
    Rss,
    /// Search engine, updates, tracker lists, GeoIP, DynDNS, .torrent URLs.
    General,
    /// Tracker/web-seed traffic.
    BitTorrent,
}

#[derive(Clone, Debug, Default)]
pub struct HttpResponse {
    pub status: u16,
    /// Final URL after redirects. For a redirect to a magnet link this is the
    /// `magnet:` URI (status = the redirect status, empty body).
    pub url: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The request ended in a redirect to a magnet link (`url` holds it).
    pub fn is_magnet_redirect(&self) -> bool {
        self.url.len() >= 7 && self.url[..7].eq_ignore_ascii_case("magnet:")
    }
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequest {
    pub url: String,
    /// Extra `Cookie` header value (e.g. from the add-torrent dialog). Only
    /// sent to the host of `url`, never to redirect targets on other hosts.
    pub cookie: Option<String>,
    /// Abort when the body exceeds this many bytes (0 = 64 MiB default).
    pub max_size: u64,
    /// Total time limit in seconds (0 = none; idle timeouts still apply).
    pub timeout_secs: u64,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self { url: url.into(), ..Default::default() }
    }
}

/// Like Qt's default and well below browsers' 20.
pub const MAX_REDIRECTS: usize = 10;
pub const DEFAULT_MAX_SIZE: u64 = 64 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const HEADERS_TIMEOUT: Duration = Duration::from_secs(60);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Extra per-call options used inside `net` (DynDNS user agent and basic auth).
#[derive(Clone, Debug, Default)]
pub(crate) struct FetchOptions {
    pub user_agent: Option<String>,
    pub basic_auth: Option<(String, String)>,
}

// ---- shared state -------------------------------------------------------------

/// Per-profile networking state: cookie jar and cached clients.
struct NetState {
    cookie_file: PathBuf,
    jar: Mutex<CookieJar>,
    save_generation: AtomicU64,
    save_lock: Mutex<u64>,
    clients: Mutex<HashMap<HttpPurpose, (ClientKey, reqwest::Client)>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ClientKey {
    proxy: Option<String>,
    ignore_ssl_errors: bool,
}

fn net_state(ctx: &CoreContext) -> Arc<NetState> {
    static STATES: OnceLock<Mutex<HashMap<PathBuf, Arc<NetState>>>> = OnceLock::new();
    let key = ctx.profile.config_dir.clone();
    let mut map = STATES.get_or_init(Default::default).lock();
    map.entry(key)
        .or_insert_with(|| {
            let cookie_file = ctx.profile.config_dir.join("cookies.json");
            let jar = match std::fs::read(&cookie_file) {
                Ok(data) => CookieJar::from_json(&data, now()),
                Err(_) => CookieJar::default(),
            };
            Arc::new(NetState {
                cookie_file,
                jar: Mutex::new(jar),
                save_generation: AtomicU64::new(0),
                save_lock: Mutex::new(0),
                clients: Mutex::new(HashMap::new()),
            })
        })
        .clone()
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl NetState {
    /// Saves the persistent cookies on a blocking thread; concurrent saves
    /// never let an older snapshot overwrite a newer one.
    fn save_cookies(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        let generation = self.save_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let data = self.jar.lock().to_json(now());
        let this = self.clone();
        rt.spawn_blocking(move || {
            let mut last = this.save_lock.lock();
            if *last >= generation {
                return;
            }
            *last = generation;
            if let Err(e) = crate::utils::fs::write_atomic_mode(&this.cookie_file, &data, 0o600) {
                tracing::warn!("cannot save cookies: {e}");
            }
        });
    }

    fn client(&self, prefs: &Preferences, purpose: HttpPurpose) -> QbtResult<reqwest::Client> {
        let key = ClientKey {
            proxy: proxy_url_checked(&prefs.connection, purpose).map_err(QbtError::network)?,
            ignore_ssl_errors: prefs.advanced.ignore_ssl_errors,
        };
        let mut clients = self.clients.lock();
        if let Some((k, c)) = clients.get(&purpose)
            && *k == key
        {
            return Ok(c.clone());
        }
        let client = build_client(&key)?;
        clients.insert(purpose, (key, client.clone()));
        Ok(client)
    }
}

fn build_client(key: &ClientKey) -> QbtResult<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .user_agent(browser_user_agent())
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .gzip(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .danger_accept_invalid_certs(key.ignore_ssl_errors)
        .danger_accept_invalid_hostnames(key.ignore_ssl_errors);
    b = match &key.proxy {
        Some(url) => {
            let proxy = reqwest::Proxy::all(url.as_str())
                .map_err(|_| QbtError::network("invalid proxy configuration"))?;
            b.proxy(proxy)
        }
        // Never pick up proxies from the environment: "no proxy" means direct.
        None => b.no_proxy(),
    };
    b.build().map_err(|e| QbtError::network(format!("cannot create HTTP client: {}", error_chain(&e))))
}

// ---- proxy -------------------------------------------------------------------------

/// Proxy URL (with credentials) for `purpose`, or `None` for a direct
/// connection. Also used for the search engine's Python environment.
pub fn proxy_url(prefs: &Preferences, purpose: HttpPurpose) -> Option<String> {
    proxy_url_checked(&prefs.connection, purpose).ok().flatten()
}

/// `Err` when the proxy is enabled for `purpose` but its settings are invalid.
fn proxy_url_checked(c: &ConnectionPrefs, purpose: HttpPurpose) -> Result<Option<String>, String> {
    let enabled = match purpose {
        HttpPurpose::Rss => c.proxy_rss,
        HttpPurpose::General => c.proxy_misc,
        HttpPurpose::BitTorrent => c.proxy_bittorrent,
    };
    if !enabled || c.proxy_type == ProxyType::None {
        return Ok(None);
    }
    let scheme = match (c.proxy_type, c.proxy_hostname_lookup) {
        (ProxyType::Http, _) => "http",
        (ProxyType::Socks5, true) => "socks5h",
        (ProxyType::Socks5, false) => "socks5",
        (ProxyType::Socks4, true) => "socks4a",
        (ProxyType::Socks4, false) => "socks4",
        (ProxyType::None, _) => unreachable!(),
    };
    let host = c.proxy_ip.trim();
    if host.is_empty() || !(1..=65535).contains(&c.proxy_port) {
        return Err("The proxy is enabled but its host or port is invalid".to_string());
    }
    let host = if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_string() };
    let mut url = Url::parse(&format!("{scheme}://{host}:{}", c.proxy_port))
        .map_err(|_| "The proxy is enabled but its host is invalid".to_string())?;
    if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() || url.host_str().is_none() {
        return Err("The proxy is enabled but its host is invalid".to_string());
    }
    // SOCKS4 has no password authentication.
    if c.proxy_auth_enabled && c.proxy_type != ProxyType::Socks4 && !c.proxy_username.is_empty() {
        let user_ok = url.set_username(c.proxy_username.trim()).is_ok();
        let pass_ok = url.set_password(Some(c.proxy_password.trim())).is_ok();
        if !user_ok || !pass_ok {
            return Err("The proxy credentials cannot be used".to_string());
        }
    }
    Ok(Some(url.to_string().trim_end_matches('/').to_string()))
}

// ---- user agent ------------------------------------------------------------------------

/// qBittorrent's browser disguise (`getBrowserUserAgent`): Firefox with a
/// version that advances monthly from 125 (2024-04-16).
pub fn browser_user_agent() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| browser_user_agent_at(now()))
}

fn browser_user_agent_at(unix: i64) -> String {
    const BASE: i64 = 1_713_225_600; // 2024-04-16T00:00:00Z
    const MONTH: i64 = 2_629_746; // std::chrono::months
    let version = 125 + (unix - BASE).max(0) / MONTH;
    format!("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:{version}.0) Gecko/20100101 Firefox/{version}.0")
}

/// User agent for requests that identify the application (DynDNS).
pub fn app_user_agent() -> String {
    format!("qBit NeoRS/{}", env!("CARGO_PKG_VERSION"))
}

// ---- requests ----------------------------------------------------------------------------

/// URL without user info, for messages.
fn redact(url: &Url) -> String {
    let mut u = url.clone();
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.to_string()
}

fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let part = s.to_string();
        if !msg.contains(&part) {
            msg.push_str(": ");
            msg.push_str(&part);
        }
        src = s.source();
    }
    msg
}

fn request_error(url: &Url, e: reqwest::Error) -> QbtError {
    let e = e.without_url();
    let reason = if e.is_timeout() {
        "The connection to the remote server timed out".to_string()
    } else {
        scrub_userinfo(&error_chain(&e))
    };
    QbtError::network(format!("{}: {reason}", redact(url)))
}

/// Removes `user:password@` from any URL embedded in a message (defence in
/// depth: proxy URLs carry credentials).
fn scrub_userinfo(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let end = rest.find(|c: char| c == '/' || c.is_whitespace() || c == '"' || c == '\'').unwrap_or(rest.len());
        if let Some(at) = rest[..end].rfind('@') {
            out.push_str("***@");
            rest = &rest[at + 1..];
        }
    }
    out.push_str(rest);
    out
}

fn parse_http_url(s: &str) -> QbtResult<Url> {
    let url = Url::parse(s.trim()).map_err(|_| QbtError::invalid(format!("invalid URL: {}", s.trim())))?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some_and(|h| !h.is_empty()) => Ok(url),
        _ => Err(QbtError::invalid(format!("unsupported URL: {}", redact(&url)))),
    }
}

/// Where the body goes.
enum Sink<'a> {
    Memory(Vec<u8>),
    File(&'a mut std::fs::File),
}

impl Sink<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Sink::Memory(v) => {
                v.extend_from_slice(data);
                Ok(())
            }
            Sink::File(f) => std::io::Write::write_all(f, data),
        }
    }
}

fn size_error(size: u64, limit: u64) -> QbtError {
    QbtError::network(format!(
        "The file size ({}) exceeds the download limit ({})",
        crate::utils::misc::friendly_unit(size as i64, false),
        crate::utils::misc::friendly_unit(limit as i64, false)
    ))
}

/// Performs a GET request. Non-2xx responses are returned as `Ok` with their
/// status; transport failures are `QbtError::Network`.
pub async fn fetch(ctx: &CoreContext, req: HttpRequest, purpose: HttpPurpose) -> QbtResult<HttpResponse> {
    fetch_with(ctx, req, purpose, FetchOptions::default()).await
}

pub(crate) async fn fetch_with(
    ctx: &CoreContext,
    req: HttpRequest,
    purpose: HttpPurpose,
    opts: FetchOptions,
) -> QbtResult<HttpResponse> {
    let mut sink = Sink::Memory(Vec::new());
    let mut resp = run_with_timeout(ctx, &req, purpose, &opts, &mut sink).await?;
    if let Sink::Memory(body) = sink {
        resp.body = body;
    }
    Ok(resp)
}

/// Downloads into `dest` (written atomically: temporary file + rename, and
/// quarantined when "mark of the web" is on). The file is only created for
/// 2xx responses; the returned body is always empty.
pub async fn fetch_to_file(
    ctx: &CoreContext,
    req: HttpRequest,
    purpose: HttpPurpose,
    dest: &Path,
) -> QbtResult<HttpResponse> {
    use std::os::unix::fs::OpenOptionsExt;

    let dir = dest.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let file_name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("download");
    let tmp = dir.join(format!(".{file_name}.{:016x}.part", rand::random::<u64>()));
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o644).open(&tmp)?;
    let result = async {
        let mut sink = Sink::File(&mut file);
        let resp = run_with_timeout(ctx, &req, purpose, &FetchOptions::default(), &mut sink).await?;
        if (200..300).contains(&resp.status) {
            file.sync_all()?;
            std::fs::rename(&tmp, dest)?;
            let _ = crate::utils::os::apply_mark_of_the_web(ctx, dest);
        }
        QbtResult::Ok(resp)
    }
    .await;
    let _ = std::fs::remove_file(&tmp);
    result
}

async fn run_with_timeout(
    ctx: &CoreContext,
    req: &HttpRequest,
    purpose: HttpPurpose,
    opts: &FetchOptions,
    sink: &mut Sink<'_>,
) -> QbtResult<HttpResponse> {
    let fut = run(ctx, req, purpose, opts, sink);
    if req.timeout_secs == 0 {
        return fut.await;
    }
    match tokio::time::timeout(Duration::from_secs(req.timeout_secs), fut).await {
        Ok(r) => r,
        Err(_) => Err(QbtError::network(format!(
            "{}: The connection to the remote server timed out",
            Url::parse(&req.url).map(|u| redact(&u)).unwrap_or_default()
        ))),
    }
}

async fn run(
    ctx: &CoreContext,
    req: &HttpRequest,
    purpose: HttpPurpose,
    opts: &FetchOptions,
    sink: &mut Sink<'_>,
) -> QbtResult<HttpResponse> {
    let original = parse_http_url(&req.url)?;
    let state = net_state(ctx);
    let client = state.client(&ctx.prefs.current(), purpose)?;
    let limit = if req.max_size == 0 { DEFAULT_MAX_SIZE } else { req.max_size };
    let mut url = original.clone();
    let mut redirects = 0usize;

    loop {
        let mut rb = client.get(url.clone());
        // qBittorrent spoofs the referer as the URL itself (some trackers
        // require it); user info never leaves in a header.
        rb = rb.header(REFERER, redact(&url));
        if let Some(ua) = &opts.user_agent {
            rb = rb.header(USER_AGENT, ua.as_str());
        }
        // Caller-supplied credentials (basic auth, extra cookie) only go to
        // the original host and never over a downgraded (https -> http) hop.
        let same_origin = url.host_str() == original.host_str()
            && url.port_or_known_default() == original.port_or_known_default()
            && !(original.scheme() == "https" && url.scheme() != "https");
        if let Some((user, pass)) = &opts.basic_auth
            && same_origin
        {
            rb = rb.basic_auth(user, Some(pass));
        }
        let mut cookie = state.jar.lock().header_for(&url, now());
        if let Some(extra) = req.cookie.as_deref().map(str::trim).filter(|c| !c.is_empty())
            && same_origin
        {
            cookie = Some(match cookie {
                Some(c) => format!("{c}; {extra}"),
                None => extra.to_string(),
            });
        }
        if let Some(c) = cookie {
            let value = reqwest::header::HeaderValue::from_str(&c)
                .map_err(|_| QbtError::invalid("invalid characters in the cookie"))?;
            rb = rb.header(COOKIE, value);
        }

        let resp = match tokio::time::timeout(HEADERS_TIMEOUT, rb.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(request_error(&url, e)),
            Err(_) => {
                return Err(QbtError::network(format!("{}: The connection to the remote server timed out", redact(&url))));
            }
        };

        let set_cookie: Vec<String> =
            resp.headers().get_all(SET_COOKIE).iter().filter_map(|v| v.to_str().ok().map(str::to_string)).collect();
        if !set_cookie.is_empty() {
            let changed = state.jar.lock().store_response(&url, &set_cookie, now());
            if changed {
                state.save_cookies(&ctx.rt);
            }
        }

        let status = resp.status();
        if status.is_redirection()
            && let Some(location) = resp.headers().get(LOCATION)
        {
            let location = location.to_str().map_err(|_| QbtError::network("invalid redirect location"))?;
            let next = url
                .join(location.trim())
                .map_err(|_| QbtError::network(format!("{}: invalid redirect location", redact(&url))))?;
            if next.scheme().eq_ignore_ascii_case("magnet") {
                return Ok(HttpResponse {
                    status: status.as_u16(),
                    url: next.to_string(),
                    content_type: String::new(),
                    body: Vec::new(),
                });
            }
            if next.scheme() != "http" && next.scheme() != "https" {
                return Err(QbtError::network(format!(
                    "Redirect to unsupported or dangerous protocol: '{}'.",
                    next.scheme()
                )));
            }
            redirects += 1;
            if redirects > MAX_REDIRECTS {
                return Err(QbtError::network(format!("Exceeded max redirections ({MAX_REDIRECTS})")));
            }
            url = next;
            continue;
        }

        let content_type = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // A compressed body's Content-Length is not the decoded size; the
        // streaming check below covers it.
        if let Some(len) = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            && len > limit
        {
            return Err(size_error(len, limit));
        }
        // Callers log this URL: never keep user info (feed credentials).
        let final_url = redact(resp.url());
        let mut resp = resp;
        let mut received = 0u64;
        loop {
            let chunk = match tokio::time::timeout(IDLE_TIMEOUT, resp.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(request_error(&url, e)),
                Err(_) => {
                    return Err(QbtError::network(format!(
                        "{}: The connection to the remote server timed out",
                        redact(&url)
                    )));
                }
            };
            received += chunk.len() as u64;
            if received > limit {
                return Err(size_error(received, limit));
            }
            sink.write(&chunk)?;
        }
        return Ok(HttpResponse { status: status.as_u16(), url: final_url, content_type, body: Vec::new() });
    }
}

// ---- cookies ---------------------------------------------------------------------------

/// All cookies of the jar (`app/cookies`, "Manage Cookies").
pub fn cookies(ctx: &CoreContext) -> Vec<Cookie> {
    net_state(ctx).jar.lock().all()
}

/// Replaces the cookie jar (`app/setCookies`) and saves it.
pub fn set_cookies(ctx: &CoreContext, cookies: Vec<Cookie>) -> QbtResult<()> {
    let state = net_state(ctx);
    state.jar.lock().set_all(cookies)?;
    state.save_cookies(&ctx.rt);
    Ok(())
}

#[cfg(test)]
mod tests;
