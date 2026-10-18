//! WebAPI v2 server (qBittorrent `src/webui`): HTTP/HTTPS server, auth
//! (session cookie, API key, localhost/subnet bypass, brute-force ban),
//! security headers (clickjacking, CSRF, host header validation, custom
//! headers, reverse proxy), every `/api/v2/*` controller, and the bundled
//! qBittorrent WebUI static files (or an alternative WebUI folder).
//! Started/stopped/reconfigured live from the WebUI preferences.
//!
//! Threat model and mitigations: docs/SECURITY.md (WebUI section).

pub(crate) mod api;
pub(crate) mod assets;
pub(crate) mod clientdata;
pub mod http;
pub mod password;
pub(crate) mod security;
pub(crate) mod server;
pub(crate) mod session;
pub mod subnet;
pub(crate) mod sync;
#[cfg(test)]
mod e2e_tests;
pub(crate) mod translate;
pub(crate) mod webapp;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::app::App;
use crate::context::CoreContext;
use crate::prefs::Preferences;

use self::server::RunningServer;
use self::webapp::WebApp;

/// State of the WebUI server, for front ends (status line, nox banner).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct WebUiStatus {
    /// The WebUI should run (preference or forced by the daemon).
    pub enabled: bool,
    /// The server is listening.
    pub running: bool,
    pub https: bool,
    /// Configured listen address (`*` = all).
    pub address: String,
    pub port: u16,
    /// URL to open, e.g. `http://localhost:8080`.
    pub url: String,
    /// Last configuration error, empty if none.
    pub error: String,
    pub username: String,
    /// Password generated for this run when none is configured (empty when
    /// a password is set).
    pub temporary_password: String,
}

/// What the listener needs; a change restarts it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ServerSpec {
    address: String,
    port: u16,
    https: bool,
    cert: Vec<u8>,
    key: Vec<u8>,
}

struct TempPassword {
    plain: String,
    hash: String,
}

pub struct WebUiController {
    ctx: CoreContext,
    web: Arc<WebApp>,
    force_enabled: AtomicBool,
    kick: Notify,
    status: Mutex<WebUiStatus>,
    temp_password: Mutex<Option<TempPassword>>,
    stop_tx: watch::Sender<bool>,
    manager: Mutex<Option<JoinHandle<()>>>,
}

impl WebUiController {
    pub fn new(ctx: &CoreContext) -> Arc<Self> {
        let (stop_tx, _) = watch::channel(false);
        Arc::new(Self {
            ctx: ctx.clone(),
            web: WebApp::new(ctx),
            force_enabled: AtomicBool::new(false),
            kick: Notify::new(),
            status: Mutex::new(WebUiStatus::default()),
            temp_password: Mutex::new(None),
            stop_tx,
            manager: Mutex::new(None),
        })
    }

    /// Gives the controller access to the facade (called once after start-up)
    /// and starts following the WebUI preferences.
    pub fn attach(&self, app: &Arc<App>) {
        self.web.set_app(Arc::downgrade(app));
        let me = app.webui.clone();
        let handle = self.ctx.rt.spawn(me.run());
        *self.manager.lock() = Some(handle);
    }

    /// Runs the WebUI even if the preference is off (headless daemon).
    pub fn set_force_enabled(&self, force: bool) {
        self.force_enabled.store(force, Ordering::Relaxed);
        self.kick.notify_one();
    }

    pub fn status(&self) -> WebUiStatus {
        self.status.lock().clone()
    }

    pub async fn stop(&self) {
        self.stop_tx.send_replace(true);
        let handle = self.manager.lock().take();
        if let Some(h) = handle {
            let _ = h.await;
        }
    }

    async fn run(self: Arc<Self>) {
        let mut prefs_rx = self.ctx.prefs.subscribe();
        let mut stop_rx = self.stop_tx.subscribe();
        let mut current: Option<(ServerSpec, RunningServer)> = None;
        loop {
            if *stop_rx.borrow() {
                break;
            }
            let prefs = prefs_rx.borrow_and_update().clone();
            self.reconfigure(&prefs, &mut current).await;
            tokio::select! {
                r = prefs_rx.changed() => if r.is_err() { break },
                _ = self.kick.notified() => {}
                _ = stop_rx.changed() => break,
            }
        }
        if let Some((_, s)) = current.take() {
            s.stop().await;
        }
        self.web.clear_sessions();
        self.status.lock().running = false;
    }

    /// Hash of the temporary password, generated once per run while no
    /// password is configured and discarded as soon as one is set.
    async fn temp_password_hash(&self, prefs: &Preferences) -> Option<String> {
        if !prefs.webui.web_ui_password_pbkdf2.is_empty() {
            *self.temp_password.lock() = None;
            return None;
        }
        if let Some(t) = self.temp_password.lock().as_ref() {
            return Some(t.hash.clone());
        }
        let plain = password::generate_password(password::TEMP_PASSWORD_LEN);
        let p = plain.clone();
        let hash = tokio::task::spawn_blocking(move || password::hash_password(&p)).await.ok()?;
        self.ctx.logger.warning(
            "The WebUI administrator password was not set. A temporary password was generated for this session \
             (shown by the application / printed by the daemon). You should set your own password in program preferences.",
        );
        *self.temp_password.lock() = Some(TempPassword { plain, hash: hash.clone() });
        Some(hash)
    }

    async fn reconfigure(&self, prefs: &Preferences, current: &mut Option<(ServerSpec, RunningServer)>) {
        let w = &prefs.webui;
        let enabled = w.web_ui_enabled || self.force_enabled.load(Ordering::Relaxed);
        let port = match self.ctx.config.webui_port_override {
            0 => u16::try_from(w.web_ui_port).ok().filter(|p| *p != 0).unwrap_or(8080),
            p => p,
        };
        let temp = if enabled { self.temp_password_hash(prefs).await } else { None };
        self.web.configure(port, w.use_https, temp);

        let address = if w.web_ui_address.trim().is_empty() { "*".to_owned() } else { w.web_ui_address.trim().to_owned() };
        let mut st = WebUiStatus {
            enabled,
            running: false,
            https: w.use_https,
            address: address.clone(),
            port,
            url: url_for(&address, port, w.use_https),
            error: String::new(),
            username: w.web_ui_username.clone(),
            temporary_password: self.temp_password.lock().as_ref().map(|t| t.plain.clone()).unwrap_or_default(),
        };

        if !enabled {
            if let Some((_, s)) = current.take() {
                s.stop().await;
                self.web.clear_sessions();
            }
            *self.status.lock() = st;
            return;
        }

        let (cert, key) = if w.use_https {
            match (read_limited(&w.web_ui_https_cert_path), read_limited(&w.web_ui_https_key_path)) {
                (Ok(c), Ok(k)) => (c, k),
                (Err(e), _) | (_, Err(e)) => {
                    // Fail closed: never fall back to plain HTTP.
                    self.fail(current, st, format!("HTTPS setup failed: {e}")).await;
                    return;
                }
            }
        } else {
            (Vec::new(), Vec::new())
        };
        let spec = ServerSpec { address: address.clone(), port, https: w.use_https, cert, key };
        if let Some((s, running)) = current.as_ref()
            && *s == spec
        {
            st.running = true;
            st.port = running.local_addr.port();
            *self.status.lock() = st;
            return;
        }
        if let Some((_, s)) = current.take() {
            s.stop().await;
        }

        let tls = if spec.https {
            match server::tls_config(&spec.cert, &spec.key) {
                Ok(c) => Some(c),
                Err(e) => {
                    self.fail(current, st, format!("HTTPS setup failed: {e}")).await;
                    return;
                }
            }
        } else {
            None
        };
        let started = server::bind(&spec.address, spec.port)
            .and_then(|l| RunningServer::start(l, self.web.clone(), tls.clone()).map_err(|e| e.to_string()));
        match started {
            Ok(running) => {
                if spec.https {
                    self.ctx.logger.normal("WebUI: HTTPS setup successful");
                }
                self.ctx.logger.normal(format!("WebUI: Now listening on IP: {}, port: {}", spec.address, spec.port));
                st.running = true;
                *self.status.lock() = st;
                *current = Some((spec, running));
            }
            Err(e) => {
                let msg = format!("Unable to bind to IP: {}, port: {}. Reason: {e}", spec.address, spec.port);
                self.fail(current, st, msg).await;
            }
        }
    }

    async fn fail(&self, current: &mut Option<(ServerSpec, RunningServer)>, mut st: WebUiStatus, error: String) {
        if let Some((_, s)) = current.take() {
            s.stop().await;
        }
        self.ctx.logger.critical(format!("WebUI: {error}"));
        st.running = false;
        st.error = error;
        *self.status.lock() = st;
    }
}

fn read_limited(path: &str) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    if path.is_empty() {
        return Err("certificate or key path is not set".into());
    }
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut data = Vec::new();
    f.take(server::MAX_SSL_FILE_SIZE + 1).read_to_end(&mut data).map_err(|e| format!("{path}: {e}"))?;
    if data.len() as u64 > server::MAX_SSL_FILE_SIZE {
        return Err(format!("{path}: file too large"));
    }
    Ok(data)
}

fn url_for(address: &str, port: u16, https: bool) -> String {
    let scheme = if https { "https" } else { "http" };
    let host = match address {
        "*" | "" | "0.0.0.0" | "::" => "localhost".to_owned(),
        a if a.contains(':') && !a.starts_with('[') => format!("[{a}]"),
        a => a.to_owned(),
    };
    format!("{scheme}://{host}:{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(url_for("*", 8080, false), "http://localhost:8080");
        assert_eq!(url_for("::1", 1, true), "https://[::1]:1");
        assert_eq!(url_for("192.168.1.2", 80, false), "http://192.168.1.2:80");
    }
}
