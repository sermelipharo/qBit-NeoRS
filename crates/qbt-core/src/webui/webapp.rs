//! The WebUI application (qBittorrent `WebApplication`): per-request
//! pipeline (method check, CSRF, Host validation, client address, session /
//! API key / Basic auth, routing, method filter, security headers) and the
//! state shared by every connection.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use axum::body::Body;
use base64::Engine as _;
use bytes::Bytes;
use http::header::{self, HeaderName, HeaderValue};
use http::{HeaderMap, Method, Request, Response, StatusCode};
use http_body_util::BodyExt as _;
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::api::{self, ApiError, Call, Methods, Reply};
use super::api::CreatorExtras;
use super::assets::{self, AltCache, EmbeddedCache, Substitutions};
use super::clientdata::ClientDataStorage;
use super::http::{accepts_gzip, authorization, cookie_value, gunzip, gzip, http_date, parse_body, parse_query};
use super::password::{slow_equals, verify_password};
use super::security::{self, BypassPolicy, CsrfVerdict, HostVerdict, sanitize_for_log};
use super::session::{BanList, SessionStore, WebSession};
use super::subnet::{Subnet, any_contains};
use crate::app::App;
use crate::context::CoreContext;
use crate::prefs::Preferences;

/// qBittorrent's `MAX_CONTENT_SIZE` for request bodies.
pub const MAX_BODY: usize = 64 * 1024 * 1024;
/// Body limit before authentication (login form).
const MAX_LOGIN_BODY: usize = 64 * 1024;
/// Total memory all in-flight request bodies may use.
const BODY_BUDGET_KIB: usize = 512 * 1024;
const BODY_TIMEOUT: Duration = Duration::from_secs(120);
/// Login bodies are tiny and come before authentication: a short deadline
/// keeps unauthenticated clients from holding connections by trickling them.
const LOGIN_BODY_TIMEOUT: Duration = Duration::from_secs(10);
/// Concurrent PBKDF2 verifications (each takes ~0.1 s of CPU).
const AUTH_SLOTS: usize = 2;
const SESSION_COOKIE_PREFIX: &str = "QBT_SID_";
const BANNED_MESSAGE: &str = "Your IP address has been banned after too many failed authentication attempts.";
/// Bounds of small shared maps.
const MAX_SSL_ENTRIES: usize = 4096;
const MAX_CREATOR_ENTRIES: usize = 512;

/// Connection facts attached to every request.
#[derive(Clone, Copy, Debug)]
pub struct ConnInfo {
    /// Socket peer (canonical: IPv4-mapped IPv6 is IPv4).
    pub peer: IpAddr,
    /// Local address the request arrived on (canonical).
    pub local: SocketAddr,
    pub tls: bool,
}

/// Request-pipeline settings derived from the preferences.
pub struct WebConfig {
    pub generation: u64,
    pub cookie_name: String,
    pub username: String,
    pub password_hash: String,
    pub local_auth_required: bool,
    pub whitelist_enabled: bool,
    pub whitelist: Vec<Subnet>,
    pub session_timeout: Option<Duration>,
    pub domains: Vec<String>,
    pub csrf: bool,
    pub secure_cookie: bool,
    pub host_validation: bool,
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// Trusted reverse proxies (`None` = reverse proxy support off).
    pub trusted_proxies: Option<Vec<Subnet>>,
    pub api_key: Option<String>,
    pub alt_ui: Option<PathBuf>,
    pub lang: String,
    pub max_auth_fail: i64,
    pub ban_duration: Duration,
}

impl WebConfig {
    /// Builds the configuration; returns warnings about ignored settings.
    pub fn build(p: &Preferences, port: u16, https: bool, temp_hash: Option<&str>, generation: u64) -> (Self, Vec<String>) {
        let w = &p.webui;
        let (custom, warnings) = if w.web_ui_use_custom_http_headers_enabled {
            security::parse_custom_headers(&w.web_ui_custom_http_headers)
        } else {
            (Vec::new(), Vec::new())
        };
        let alt_ui = w.alternative_webui_enabled.then(|| PathBuf::from(&w.alternative_webui_path));
        let trusted: Vec<Subnet> = super::subnet::parse_list(w.web_ui_reverse_proxies_list.split(';'));
        let password_hash = if w.web_ui_password_pbkdf2.is_empty() {
            temp_hash.unwrap_or_default().to_owned()
        } else {
            w.web_ui_password_pbkdf2.clone()
        };
        let cfg = Self {
            generation,
            cookie_name: format!("{SESSION_COOKIE_PREFIX}{port}"),
            username: w.web_ui_username.clone(),
            password_hash,
            local_auth_required: !w.bypass_local_auth,
            whitelist_enabled: w.bypass_auth_subnet_whitelist_enabled,
            whitelist: super::subnet::parse_list(w.bypass_auth_subnet_whitelist.iter().map(String::as_str)),
            session_timeout: (w.web_ui_session_timeout > 0).then(|| capped_secs(w.web_ui_session_timeout)),
            domains: w.web_ui_domain_list.split(';').map(str::trim).filter(|d| !d.is_empty()).map(str::to_owned).collect(),
            csrf: w.web_ui_csrf_protection_enabled,
            secure_cookie: w.web_ui_secure_cookie_enabled,
            host_validation: w.web_ui_host_header_validation_enabled,
            headers: security::prebuilt_headers(alt_ui.is_some(), w.web_ui_clickjacking_protection_enabled, https, custom),
            trusted_proxies: (w.web_ui_reverse_proxy_enabled && !trusted.is_empty()).then_some(trusted),
            api_key: Some(w.web_ui_api_key.clone()).filter(|k| super::password::is_valid_api_key(k)),
            alt_ui,
            lang: assets::lang_code(&p.behavior.locale),
            max_auth_fail: w.web_ui_max_auth_fail_count,
            ban_duration: capped_secs(w.web_ui_ban_duration),
        };
        (cfg, warnings)
    }
}

/// Durations from preferences are capped (100 years) so time arithmetic can
/// never overflow.
fn capped_secs(v: i64) -> Duration {
    Duration::from_secs(v.clamp(0, 100 * 365 * 86_400) as u64)
}

/// PEM SSL parameters of an SSL torrent (stored here; the engine does not
/// use them yet).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SslParameters {
    pub certificate: String,
    pub private_key: String,
    pub dh_params: String,
}

impl SslParameters {
    pub fn is_valid(&self) -> bool {
        use rustls_pki_types::pem::PemObject as _;
        use rustls_pki_types::{CertificateDer, PrivateKeyDer};
        let cert_ok = CertificateDer::pem_slice_iter(self.certificate.as_bytes()).next().is_some_and(|c| c.is_ok());
        let key_ok = PrivateKeyDer::from_pem_slice(self.private_key.as_bytes()).is_ok();
        cert_ok && key_ok && !self.dh_params.trim().is_empty()
    }
}

/// A response being built.
struct Out {
    status: StatusCode,
    content_type: Option<&'static str>,
    body: Bytes,
    gzipped: bool,
    headers: Vec<(HeaderName, HeaderValue)>,
    close: bool,
}

impl Out {
    fn new(status: StatusCode) -> Self {
        Self { status, content_type: None, body: Bytes::new(), gzipped: false, headers: Vec::new(), close: false }
    }

    fn text(status: StatusCode, text: impl Into<String>) -> Self {
        let mut o = Self::new(status);
        o.content_type = Some("text/plain; charset=UTF-8");
        o.body = Bytes::from(text.into());
        o
    }

    fn error(e: ApiError) -> Self {
        let msg = if e.message.is_empty() { e.status.canonical_reason().unwrap_or("").to_owned() } else { e.message };
        let mut o = Self::text(e.status, msg);
        o.close = matches!(e.status, StatusCode::NOT_IMPLEMENTED | StatusCode::PAYLOAD_TOO_LARGE | StatusCode::REQUEST_TIMEOUT);
        o
    }

    fn from_reply(r: Reply) -> Self {
        match r {
            Reply::NoContent => Self::new(StatusCode::NO_CONTENT),
            Reply::Text(s) => Self::text(StatusCode::OK, s),
            Reply::Json(v) => Self::json(StatusCode::OK, &v),
            Reply::Async(v) => Self::json(StatusCode::ACCEPTED, &v),
            Reply::Bytes { data, mime, filename } => {
                let mut o = Self::new(StatusCode::OK);
                o.content_type = Some(mime);
                o.body = Bytes::from(data);
                if let Some(f) = filename {
                    let safe: String = f.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
                    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
                        o.headers.push((header::CONTENT_DISPOSITION, v));
                    }
                }
                o
            }
        }
    }

    fn json(status: StatusCode, v: &Value) -> Self {
        let mut o = Self::new(status);
        o.content_type = Some("application/json");
        o.body = Bytes::from(serde_json::to_vec(v).unwrap_or_else(|_| b"null".to_vec()));
        o
    }
}

type Outcome<T> = Result<T, Out>;

pub struct WebApp {
    ctx: CoreContext,
    app: OnceLock<Weak<App>>,
    config: RwLock<Arc<WebConfig>>,
    generation: AtomicU64,
    /// (effective port, HTTPS, temporary password hash) of the running server.
    runtime_settings: Mutex<(u16, bool, Option<String>)>,
    /// Serializes configuration updates, so that a stale snapshot never
    /// overwrites the effect of a newer preferences change.
    config_lock: Mutex<()>,
    sessions: Mutex<SessionStore>,
    bans: Mutex<BanList>,
    auth_slots: Arc<Semaphore>,
    body_budget: Arc<Semaphore>,
    clientdata: ClientDataStorage,
    ssl: Mutex<HashMap<String, SslParameters>>,
    creator: Mutex<HashMap<String, CreatorExtras>>,
    embedded_cache: Mutex<EmbeddedCache>,
    alt_cache: Arc<Mutex<AltCache>>,
    cache_id: String,
    launch_time: i64,
    last_ui: Mutex<Option<Option<PathBuf>>>,
}

impl WebApp {
    pub fn new(ctx: &CoreContext) -> Arc<Self> {
        let logger = ctx.logger.clone();
        let clientdata = ClientDataStorage::load(ctx.profile.config_dir.join("webui_clientdata.json"), move |m| {
            logger.warning(m)
        });
        let mut id = [0u8; 8];
        super::password::random_bytes(&mut id);
        let prefs = ctx.prefs.current();
        let port = u16::try_from(prefs.webui.web_ui_port).unwrap_or(8080);
        let (cfg, _) = WebConfig::build(&prefs, port, false, None, 0);
        Arc::new(Self {
            ctx: ctx.clone(),
            app: OnceLock::new(),
            config: RwLock::new(Arc::new(cfg)),
            generation: AtomicU64::new(0),
            runtime_settings: Mutex::new((port, false, None)),
            config_lock: Mutex::new(()),
            sessions: Mutex::default(),
            bans: Mutex::default(),
            auth_slots: Arc::new(Semaphore::new(AUTH_SLOTS)),
            body_budget: Arc::new(Semaphore::new(BODY_BUDGET_KIB)),
            clientdata,
            ssl: Mutex::default(),
            creator: Mutex::default(),
            embedded_cache: Mutex::default(),
            alt_cache: Arc::default(),
            cache_id: format!("{:x}", u64::from_le_bytes(id)),
            launch_time: chrono::Utc::now().timestamp(),
            last_ui: Mutex::new(None),
        })
    }

    pub fn set_app(&self, app: Weak<App>) {
        let _ = self.app.set(app);
    }

    fn app(&self) -> Option<Arc<App>> {
        self.app.get().and_then(Weak::upgrade)
    }

    pub fn config(&self) -> Arc<WebConfig> {
        self.config.read().clone()
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.ctx.rt
    }

    pub fn launch_time(&self) -> i64 {
        self.launch_time
    }

    pub fn clientdata(&self) -> &ClientDataStorage {
        &self.clientdata
    }

    pub fn log_info(&self, m: String) {
        self.ctx.logger.info(m);
    }

    pub fn log_warning(&self, m: String) {
        self.ctx.logger.warning(m);
    }

    /// Applies new preferences / server facts to the request pipeline.
    ///
    /// The configuration is always built from the latest preferences, never
    /// from an older snapshot: the server task can be slow to get here while a
    /// WebAPI change (e.g. a deleted API key) has already been applied.
    pub fn configure(&self, port: u16, https: bool, temp_hash: Option<String>) {
        let _guard = self.config_lock.lock();
        self.apply(&self.ctx.prefs.current(), port, https, temp_hash);
    }

    fn apply(&self, prefs: &Preferences, port: u16, https: bool, temp_hash: Option<String>) {
        *self.runtime_settings.lock() = (port, https, temp_hash.clone());
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (cfg, warnings) = WebConfig::build(prefs, port, https, temp_hash.as_deref(), generation);
        for w in warnings {
            self.ctx.logger.warning(w);
        }
        {
            let mut last = self.last_ui.lock();
            if last.as_ref() != Some(&cfg.alt_ui) {
                match &cfg.alt_ui {
                    None => self.ctx.logger.normal("Using built-in WebUI."),
                    Some(p) => self.ctx.logger.normal(format!("Using custom WebUI. Location: \"{}\".", p.display())),
                }
                self.alt_cache.lock().clear();
                *last = Some(cfg.alt_ui.clone());
            }
        }
        let credentials_changed = {
            let old = self.config.read();
            old.username != cfg.username || old.password_hash != cfg.password_hash
        };
        {
            let mut sessions = self.sessions.lock();
            // A new username or password logs every cookie session out, so a
            // session obtained with leaked credentials does not survive the
            // password change.
            if credentials_changed {
                sessions.clear_cookie_sessions();
            }
            sessions.retain_api_key(cfg.api_key.as_deref());
        }
        *self.config.write() = Arc::new(cfg);
    }

    /// Re-reads the preferences right away (after a WebAPI change), so the
    /// next request already sees e.g. a rotated API key.
    pub fn reload_config(&self) {
        let _guard = self.config_lock.lock();
        let (port, https, temp) = self.runtime_settings.lock().clone();
        self.apply(&self.ctx.prefs.current(), port, https, temp);
    }

    /// `app/preferences` with machine-dependent defaults resolved.
    pub fn preferences_json(&self) -> Value {
        let mut v = self.ctx.prefs.to_api_value();
        if let Some(Value::String(locale)) = v.get_mut("locale")
            && locale.is_empty()
        {
            *locale = system_locale();
        }
        if let Some(Value::String(path)) = v.get_mut("file_log_path")
            && path.is_empty()
        {
            *path = self.ctx.profile.logs_dir().to_string_lossy().into_owned();
        }
        v
    }

    /// Atomically modifies the preferences and applies them at once.
    pub async fn update_preferences(&self, f: impl FnOnce(&mut Preferences) + Send + 'static) -> Result<(), ApiError> {
        let prefs = self.ctx.prefs.clone();
        tokio::task::spawn_blocking(move || prefs.update(f))
            .await
            .map_err(|e| ApiError::internal(e.to_string()))??;
        self.reload_config();
        Ok(())
    }

    pub fn ssl_parameters(&self, hash: &str) -> SslParameters {
        self.ssl.lock().get(hash).cloned().unwrap_or_default()
    }

    pub fn set_ssl_parameters(&self, hash: &str, p: SslParameters) {
        let mut m = self.ssl.lock();
        if m.len() >= MAX_SSL_ENTRIES && !m.contains_key(hash) {
            m.clear();
        }
        m.insert(hash.to_owned(), p);
    }

    pub fn creator_extras(&self, id: &str) -> Option<CreatorExtras> {
        self.creator.lock().get(id).cloned()
    }

    pub fn set_creator_extras(&self, id: &str, x: CreatorExtras) {
        let mut m = self.creator.lock();
        if m.len() >= MAX_CREATOR_ENTRIES
            && let Some(app) = self.app()
        {
            let live: Vec<String> = app.creator_tasks().into_iter().map(|t| t.id).collect();
            m.retain(|k, _| live.contains(k));
        }
        m.insert(id.to_owned(), x);
    }

    pub fn remove_creator_extras(&self, id: &str) {
        self.creator.lock().remove(id);
    }

    /// Drops every session (server restart with a new cookie name, shutdown).
    pub fn clear_sessions(&self) {
        self.sessions.lock().clear();
    }

    // ---- Request pipeline ---------------------------------------------------------

    /// Handles one request and returns the response with security headers.
    pub async fn handle(self: Arc<Self>, req: Request<Body>, conn: ConnInfo) -> Response<Body> {
        let cfg = self.config();
        let gzip_ok = req.headers().get(header::ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(accepts_gzip);
        let out = self.process(req, &conn, &cfg).await;
        finish(out, &cfg, gzip_ok)
    }

    async fn process(self: &Arc<Self>, req: Request<Body>, conn: &ConnInfo, cfg: &WebConfig) -> Out {
        let method = req.method().clone();
        let is_get = method == Method::GET || method == Method::HEAD;
        let is_post = method == Method::POST;
        if !is_get && !is_post {
            return Out::error(ApiError::new(StatusCode::NOT_IMPLEMENTED, ""));
        }
        let (parts, body) = req.into_parts();
        let headers = &parts.headers;
        let path = String::from_utf8_lossy(&super::http::percent_decode(parts.uri.path().as_bytes(), false)).into_owned();
        if path.contains('\0') {
            return Out::error(ApiError::bad_request());
        }
        // HEAD is for static files only: on the API it would run the action
        // (upstream answers 405 too).
        if method == Method::HEAD && parse_api_path(&path).is_some() {
            return Out::error(ApiError::method_not_allowed());
        }
        let bearer = hdr(headers, header::AUTHORIZATION).and_then(|h| authorization(h, "Bearer")).map(str::to_owned);
        let peer_trusted = cfg.trusted_proxies.as_ref().is_some_and(|t| any_contains(t, conn.peer));

        // Block suspicious requests.
        if bearer.is_none() && cfg.csrf {
            let host = hdr(headers, header::HOST).unwrap_or("");
            let target = if peer_trusted { hdr(headers, "x-forwarded-host").unwrap_or(host) } else { host };
            let origin = hdr(headers, header::ORIGIN);
            let referer = hdr(headers, header::REFERER);
            match security::check_csrf(target, origin, referer) {
                CsrfVerdict::SameSite => {}
                verdict => {
                    let (name, value) = match verdict {
                        CsrfVerdict::OriginMismatch => ("Origin", origin.unwrap_or("")),
                        _ => ("Referer", referer.unwrap_or("")),
                    };
                    self.log_warning(format!(
                        "WebUI: {name} header & Target origin mismatch! Source IP: '{}'. {name} header: '{}'. Target origin: '{}'",
                        conn.peer,
                        sanitize_for_log(value),
                        sanitize_for_log(target)
                    ));
                    return Out::error(ApiError::unauthorized());
                }
            }
        }
        if cfg.host_validation {
            let host = hdr(headers, header::HOST).unwrap_or("");
            match security::validate_host(host, conn.local, &cfg.domains) {
                HostVerdict::Valid => {}
                HostVerdict::PortMismatch => {
                    self.log_warning(format!(
                        "WebUI: Invalid Host header, port mismatch. Request source IP: '{}'. Server port: '{}'. Received Host header: '{}'",
                        conn.peer,
                        conn.local.port(),
                        sanitize_for_log(host)
                    ));
                    return Out::error(ApiError::unauthorized());
                }
                HostVerdict::Invalid => {
                    self.log_warning(format!(
                        "WebUI: Invalid Host header. Request source IP: '{}'. Received Host header: '{}'",
                        conn.peer,
                        sanitize_for_log(host)
                    ));
                    return Out::error(ApiError::unauthorized());
                }
            }
        }

        let client = security::resolve_client(
            conn.peer,
            if peer_trusted { hdr(headers, "x-forwarded-for") } else { None },
            cfg.trusted_proxies.as_deref(),
        );
        let secure_origin = conn.tls
            || (peer_trusted && hdr(headers, "x-forwarded-proto").is_some_and(|p| p.trim().eq_ignore_ascii_case("https")));

        let mut set_cookie = None;
        let session = match &bearer {
            Some(key) => self.api_key_session(cfg, key),
            None => match self.cookie_session(cfg, headers, conn, client, secure_origin, &mut set_cookie).await {
                Ok(s) => s,
                Err(out) => return out,
            },
        };

        let mut out = match parse_api_path(&path) {
            None if bearer.is_some() => Out::error(ApiError::not_found()),
            None => self.serve_static(&path, session.is_some(), cfg).await,
            Some((scope, action)) => {
                let ctx = ApiRequest {
                    scope,
                    action,
                    session,
                    api_key: bearer.is_some(),
                    is_post,
                    query: parts.uri.query().unwrap_or(""),
                    headers,
                    client,
                    secure_origin,
                };
                match self.api(ctx, body, cfg, &mut set_cookie).await {
                    Ok(o) | Err(o) => o,
                }
            }
        };
        if let Some(c) = set_cookie {
            out.headers.push((header::SET_COOKIE, c));
        }
        out
    }

    fn api_key_session(&self, cfg: &WebConfig, key: &str) -> Option<Arc<WebSession>> {
        let configured = cfg.api_key.as_deref()?;
        if !slow_equals(key.as_bytes(), configured.as_bytes()) {
            return None;
        }
        Some(self.sessions.lock().api_key_session(configured, Instant::now()))
    }

    async fn cookie_session(
        &self,
        cfg: &WebConfig,
        headers: &HeaderMap,
        conn: &ConnInfo,
        client: IpAddr,
        secure_origin: bool,
        set_cookie: &mut Option<HeaderValue>,
    ) -> Outcome<Option<Arc<WebSession>>> {
        let now = Instant::now();
        let sid = hdr(headers, header::COOKIE).and_then(|c| cookie_value(c, &cfg.cookie_name));
        if let Some(sid) = sid {
            let found = self.sessions.lock().lookup(&sid, cfg.session_timeout, now);
            // A cookie can never select an API-key session.
            if let Some(s) = found.filter(|s| !s.is_api_key()) {
                if s.should_refresh_cookie(now, cfg.generation) {
                    *set_cookie = self.session_cookie(cfg, &s, secure_origin, now);
                }
                return Ok(Some(s));
            }
        }
        let policy = BypassPolicy {
            local_auth_required: cfg.local_auth_required,
            whitelist_enabled: cfg.whitelist_enabled,
            whitelist: &cfg.whitelist,
            proxied: security::has_forwarding_headers(headers),
        };
        // The Host check keeps DNS-rebinding pages from riding the bypass.
        let host = hdr(headers, header::HOST).unwrap_or("");
        if !security::is_auth_needed(&policy, conn.peer, client) && security::host_allows_bypass(host, &cfg.domains) {
            let s = self.sessions.lock().create(cfg.session_timeout, now);
            *set_cookie = self.session_cookie(cfg, &s, secure_origin, now);
            return Ok(Some(s));
        }
        let basic = hdr(headers, header::AUTHORIZATION).and_then(|h| authorization(h, "Basic"));
        if let Some(cred) = basic {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(cred.as_bytes())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let ok = match decoded.split_once(':') {
                Some((user, pass)) if !user.is_empty() => self.validate_credentials(cfg, client, user, pass).await?,
                _ => false,
            };
            if !ok {
                return Err(Out::error(ApiError::unauthorized()));
            }
            let s = self.sessions.lock().create(cfg.session_timeout, now);
            *set_cookie = self.session_cookie(cfg, &s, secure_origin, now);
            return Ok(Some(s));
        }
        Ok(None)
    }

    /// qBittorrent's `validateCredentials`: ban check, constant-time username
    /// comparison, PBKDF2 verification (always performed, bounded
    /// concurrency), failure counting.
    async fn validate_credentials(&self, cfg: &WebConfig, client: IpAddr, user: &str, pass: &str) -> Outcome<bool> {
        let now = Instant::now();
        // The attempt is reserved before the (slow, queued) verification, so
        // concurrent requests cannot all pass the ban check at once.
        if !self.bans.lock().begin_attempt(client, cfg.max_auth_fail, now) {
            self.log_warning(format!(
                "WebAPI login failure. Reason: IP has been banned, IP: {client}, username: {}",
                sanitize_for_log(user)
            ));
            return Err(Out::error(ApiError::forbidden(BANNED_MESSAGE)));
        }
        let user_ok = slow_equals(user.as_bytes(), cfg.username.as_bytes());
        let hash = cfg.password_hash.clone();
        let pass = pass.to_owned();
        let attempt = AttemptGuard { bans: &self.bans, client };
        let pass_ok = {
            let _slot = self.auth_slots.clone().acquire_owned().await.map_err(|_| Out::error(ApiError::internal("")))?;
            tokio::task::spawn_blocking(move || !hash.is_empty() && verify_password(&hash, &pass)).await.unwrap_or(false)
        };
        drop(attempt);
        if user_ok && pass_ok {
            self.bans.lock().clear(client);
            self.log_info(format!("WebAPI login success. IP: {client}"));
            return Ok(true);
        }
        let count = {
            let mut b = self.bans.lock();
            b.record_failure(client, cfg.max_auth_fail, cfg.ban_duration, Instant::now());
            b.failed_attempts(client)
        };
        self.log_warning(format!(
            "WebAPI login failure. Reason: invalid credentials, attempt count: {count}, IP: {client}, username: {}",
            sanitize_for_log(user)
        ));
        Ok(false)
    }

    /// `Set-Cookie` for a session: HttpOnly, Path=/, SameSite=Strict with
    /// CSRF protection, Secure on trustworthy origins when enabled.
    fn session_cookie(&self, cfg: &WebConfig, s: &WebSession, secure_origin: bool, now: Instant) -> Option<HeaderValue> {
        let lifetime = cfg.session_timeout.unwrap_or(Duration::from_secs(365 * 86_400));
        let expires = chrono::Duration::from_std(lifetime)
            .ok()
            .and_then(|d| chrono::Utc::now().checked_add_signed(d))
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC);
        let secure = cfg.secure_cookie && secure_origin;
        let mut c = format!("{}={}; Expires={}; HttpOnly; Path=/", cfg.cookie_name, s.id(), http_date(expires));
        if secure {
            c.push_str("; Secure");
        }
        if cfg.csrf {
            c.push_str("; SameSite=Strict");
        } else if secure {
            c.push_str("; SameSite=None");
        }
        s.cookie_sent(now, lifetime, cfg.generation);
        HeaderValue::from_str(&c).ok()
    }

    fn expired_cookie(cfg: &WebConfig) -> Option<HeaderValue> {
        let yesterday = chrono::Utc::now() - chrono::Duration::days(1);
        HeaderValue::from_str(&format!("{}=; Expires={}; HttpOnly; Path=/", cfg.cookie_name, http_date(yesterday))).ok()
    }

    async fn serve_static(&self, path: &str, has_session: bool, cfg: &WebConfig) -> Out {
        let rel = match assets::normalize_request_path(path) {
            Ok(r) => r,
            Err(e) => return Out::error(e),
        };
        let result = match &cfg.alt_ui {
            None => {
                let subs = Substitutions { lang: &cfg.lang, cache_id: &self.cache_id };
                assets::serve_embedded(&rel, has_session, &subs, &self.embedded_cache)
            }
            Some(root) => {
                let (root, lang, cache_id, cache) = (root.clone(), cfg.lang.clone(), self.cache_id.clone(), self.alt_cache.clone());
                tokio::task::spawn_blocking(move || {
                    let subs = Substitutions { lang: &lang, cache_id: &cache_id };
                    assets::serve_alternative(&root, &rel, has_session, &subs, &cache)
                })
                .await
                .unwrap_or_else(|e| Err(ApiError::internal(e.to_string())))
            }
        };
        match result {
            Ok(f) => {
                let mut o = Out::new(StatusCode::OK);
                o.content_type = Some(f.mime);
                o.body = f.body;
                o.gzipped = f.gzipped;
                o.headers.push((header::CACHE_CONTROL, HeaderValue::from_static(assets::cache_control(f.mime))));
                o
            }
            Err(e) => Out::error(e),
        }
    }

    async fn api(self: &Arc<Self>, r: ApiRequest<'_>, body: Body, cfg: &WebConfig, set_cookie: &mut Option<HeaderValue>) -> Outcome<Out> {
        let is_login = r.scope == "auth" && r.action == "login";
        if r.session.is_none() && !is_login {
            return Err(Out::error(ApiError::forbidden("")));
        }
        if r.scope == "auth" {
            if r.api_key {
                return Err(Out::error(ApiError::forbidden("")));
            }
        } else if !api::SESSION_SCOPES.contains(&r.scope) {
            return Err(Out::error(ApiError::not_found()));
        }
        if api::method_filter(r.scope, r.action) == Some(Methods::PostOnly) && !r.is_post {
            return Err(Out::error(ApiError::method_not_allowed()));
        }

        let limit = if is_login { MAX_LOGIN_BODY } else { MAX_BODY };
        let (params, files, _budget) = if r.is_post {
            let deadline = if is_login { LOGIN_BODY_TIMEOUT } else { BODY_TIMEOUT };
            let (bytes, budget) = self.read_body(body, r.headers, limit, deadline).await?;
            let ct = hdr(r.headers, header::CONTENT_TYPE).unwrap_or("");
            let (p, f) = parse_body(ct, &bytes).map_err(|_| Out::error(ApiError::bad_request()))?;
            (p, f, budget)
        } else {
            (parse_query(r.query), Default::default(), None)
        };

        if r.scope == "auth" {
            return Ok(self.auth(r.action, &params, r.session, r.client, r.secure_origin, cfg, set_cookie).await);
        }
        let Some(app) = self.app() else {
            return Err(Out::error(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "")));
        };
        let session = r.session.expect("checked above");
        let mut call = Call { web: self, app, session, params, files };
        Ok(match api::dispatch(&mut call, r.scope, r.action).await {
            Ok(reply) => Out::from_reply(reply),
            Err(e) => Out::error(e),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn auth(
        &self,
        action: &str,
        params: &super::http::Params,
        session: Option<Arc<WebSession>>,
        client: IpAddr,
        secure_origin: bool,
        cfg: &WebConfig,
        set_cookie: &mut Option<HeaderValue>,
    ) -> Out {
        match action {
            "login" => {
                if session.is_some() {
                    return Out::new(StatusCode::NO_CONTENT);
                }
                match self.validate_credentials(cfg, client, params.str("username"), params.str("password")).await {
                    Err(out) => out,
                    Ok(false) => Out::error(ApiError::unauthorized()),
                    Ok(true) => {
                        let now = Instant::now();
                        let s = self.sessions.lock().create(cfg.session_timeout, now);
                        *set_cookie = self.session_cookie(cfg, &s, secure_origin, now);
                        Out::new(StatusCode::NO_CONTENT)
                    }
                }
            }
            "logout" => {
                if let Some(s) = session {
                    self.sessions.lock().remove(s.id());
                }
                *set_cookie = Self::expired_cookie(cfg);
                Out::text(StatusCode::OK, "")
            }
            _ => Out::error(ApiError::endpoint_not_found()),
        }
    }

    /// Reads a request body within `limit`, the global memory budget and a
    /// timeout.
    async fn read_body(
        &self,
        body: Body,
        headers: &HeaderMap,
        limit: usize,
        deadline: Duration,
    ) -> Outcome<(Bytes, Option<OwnedSemaphorePermit>)> {
        if let Some(len) = hdr(headers, header::CONTENT_LENGTH).and_then(|v| v.trim().parse::<u64>().ok())
            && len > limit as u64
        {
            return Err(Out::error(ApiError::payload_too_large()));
        }
        let budget = self.body_budget.clone();
        let collect = async move {
            let mut body = body;
            let mut buf = bytes::BytesMut::new();
            let mut permit: Option<OwnedSemaphorePermit> = None;
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| Out::error(ApiError::bad_request()))?;
                let Ok(data) = frame.into_data() else { continue };
                if buf.len() + data.len() > limit {
                    return Err(Out::error(ApiError::payload_too_large()));
                }
                let kib = (buf.len() + data.len()).div_ceil(1024) - buf.len().div_ceil(1024);
                if kib > 0 {
                    let p = budget
                        .clone()
                        .try_acquire_many_owned(kib as u32)
                        .map_err(|_| Out::error(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "Server is busy")))?;
                    match &mut permit {
                        Some(existing) => existing.merge(p),
                        None => permit = Some(p),
                    }
                }
                buf.extend_from_slice(&data);
            }
            Ok((buf.freeze(), permit))
        };
        match tokio::time::timeout(deadline, collect).await {
            Ok(r) => r,
            Err(_) => Err(Out::error(ApiError::new(StatusCode::REQUEST_TIMEOUT, ""))),
        }
    }
}

struct ApiRequest<'a> {
    scope: &'a str,
    action: &'a str,
    session: Option<Arc<WebSession>>,
    api_key: bool,
    is_post: bool,
    query: &'a str,
    headers: &'a HeaderMap,
    client: IpAddr,
    secure_origin: bool,
}

fn hdr(headers: &HeaderMap, name: impl http::header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn is_ident(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// `^/api/v2/(?<scope>[A-Za-z_][A-Za-z_0-9]*)/(?<action>[A-Za-z_][A-Za-z_0-9]*)$`
pub fn parse_api_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/v2/")?;
    let (scope, action) = rest.split_once('/')?;
    (is_ident(scope) && is_ident(action)).then_some((scope, action))
}

fn system_locale() -> String {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var) {
            let code = v.split(['.', '@']).next().unwrap_or("").to_owned();
            if !code.is_empty() && code != "C" && code != "POSIX" {
                return code;
            }
        }
    }
    "en".to_owned()
}

/// Compression, security headers and conversion to an HTTP response.
fn finish(mut out: Out, cfg: &WebConfig, gzip_ok: bool) -> Response<Body> {
    let compressible = !matches!(out.content_type, Some("image/png" | "image/gif"));
    if out.gzipped {
        if !gzip_ok {
            match gunzip(&out.body, assets::MAX_FILE_SIZE as usize) {
                Some(raw) => {
                    out.body = Bytes::from(raw);
                    out.gzipped = false;
                }
                None => out = Out::error(ApiError::internal("")),
            }
        }
    } else if gzip_ok && compressible && out.body.len() > 1024
        && let Some(z) = gzip(&out.body, 6)
        && z.len() + 24 < out.body.len()
    {
        out.body = Bytes::from(z);
        out.gzipped = true;
    }

    let mut resp = Response::new(Body::from(out.body));
    *resp.status_mut() = out.status;
    let h = resp.headers_mut();
    if let Some(ct) = out.content_type {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    }
    if out.gzipped {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        h.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    if out.close {
        h.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    for (k, v) in out.headers {
        h.append(k, v);
    }
    for (k, v) in &cfg.headers {
        h.insert(k.clone(), v.clone());
    }
    resp
}

/// Releases a credential check reserved with `BanList::begin_attempt`, also
/// when the request future is dropped mid-verification.
struct AttemptGuard<'a> {
    bans: &'a Mutex<BanList>,
    client: IpAddr,
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        self.bans.lock().end_attempt(self.client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths() {
        assert_eq!(parse_api_path("/api/v2/app/version"), Some(("app", "version")));
        assert_eq!(parse_api_path("/api/v2/torrents/SSLParameters"), Some(("torrents", "SSLParameters")));
        assert_eq!(parse_api_path("/api/v2/_x/y_1"), Some(("_x", "y_1")));
        for bad in ["/api/v2/app", "/api/v2/app/", "/api/v2/app/version/x", "/api/v2/1app/x", "/api/v1/app/version", "/api/v2/a-b/c", "/api/v2//x"] {
            assert_eq!(parse_api_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn ssl_parameter_validation() {
        assert!(!SslParameters::default().is_valid());
        let p = SslParameters { certificate: "x".into(), private_key: "y".into(), dh_params: "z".into() };
        assert!(!p.is_valid());
        let good = SslParameters {
            certificate: include_str!("testdata/cert.pem").into(),
            private_key: include_str!("testdata/key.pem").into(),
            dh_params: "dh".into(),
        };
        assert!(good.is_valid());
        assert!(super::super::server::tls_config(good.certificate.as_bytes(), good.private_key.as_bytes()).is_ok());
    }

    #[test]
    fn config_from_preferences() {
        let mut p = Preferences::default();
        p.webui.web_ui_domain_list = " a.com ; ;b.com".into();
        p.webui.web_ui_reverse_proxy_enabled = true;
        p.webui.web_ui_reverse_proxies_list = "10.0.0.1; bad".into();
        p.webui.web_ui_api_key = "qbt_invalid".into();
        p.webui.web_ui_session_timeout = 0;
        let (c, _) = WebConfig::build(&p, 8080, false, Some("salt:hash"), 3);
        assert_eq!(c.cookie_name, "QBT_SID_8080");
        assert_eq!(c.domains, ["a.com", "b.com"]);
        assert_eq!(c.trusted_proxies.as_ref().map(Vec::len), Some(1));
        assert!(c.api_key.is_none());
        assert!(c.session_timeout.is_none());
        assert_eq!(c.password_hash, "salt:hash");
        p.webui.web_ui_session_timeout = i64::MAX;
        p.webui.web_ui_ban_duration = i64::MAX;
        let (c, _) = WebConfig::build(&p, 8080, false, None, 3);
        assert!(c.session_timeout.unwrap() <= Duration::from_secs(100 * 365 * 86_400));
        assert!(Instant::now().checked_add(c.ban_duration).is_some());
        p.webui.web_ui_reverse_proxies_list = "garbage".into();
        let (c, _) = WebConfig::build(&p, 9090, true, None, 4);
        assert!(c.trusted_proxies.is_none());
        assert!(c.headers.iter().any(|(k, v)| k == "content-security-policy" && v.to_str().unwrap().contains("upgrade-insecure-requests")));
    }
}
