//! Dynamic DNS updater (qBittorrent `Net::DNSUpdater`, WebUI options page).
//!
//! Active while both the WebUI and "Update my dynamic domain name" are
//! enabled. Every 30 minutes the external IP is read from
//! `http://checkip.dyndns.org`; when it changed, the DynDNS or No-IP update
//! URL is called with HTTP basic authentication. Response codes are handled
//! like qBittorrent: `good`/`nochg` succeed, `911`/`dnserr` retry in 30
//! minutes, `nohost`/`badauth` stop until the credentials change, and
//! `badagent`/`!donator`/`abuse` stop for the rest of the session. The last
//! IP and check time are kept in `data/dyndns.json` so restarts do not flood
//! the service. The password is only sent in the `Authorization` header and
//! never logged.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::http::{self, FetchOptions, HttpPurpose, HttpRequest};
use crate::context::CoreContext;
use crate::model::LogLevel;
use crate::prefs::{DynDnsService, WebUiPrefs};

pub const IP_CHECK_INTERVAL_SECS: i64 = 30 * 60;
const CHECK_IP_URL: &str = "http://checkip.dyndns.org";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdaterState {
    Ok,
    /// Waiting for the user to change domain/username/password.
    InvalidCredentials,
    /// The service refused us; not retried this session.
    Fatal,
}

/// Credentials-and-state machine, independent of networking (testable).
#[derive(Debug)]
pub struct Updater {
    pub service: DynDnsService,
    pub domain: String,
    pub username: String,
    pub password: String,
    pub state: UpdaterState,
    /// The 30-minute timer runs.
    pub timer_running: bool,
    pub last_ip: Option<IpAddr>,
    /// Unix seconds of the last IP check (0 = never).
    pub last_check: i64,
    /// When the next IP check is due (Unix seconds).
    pub next_check: Option<i64>,
}

type Log<'a> = &'a mut dyn FnMut(LogLevel, String);

/// qBittorrent's domain check: `^(?:(?!\d|-)[a-zA-Z0-9\-]{1,63}\.)+[a-zA-Z]{2,}$`.
pub fn is_valid_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let (tld, rest) = labels.split_last().expect("non-empty");
    if tld.len() < 2 || !tld.chars().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    rest.iter().all(|l| {
        (1..=63).contains(&l.len())
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !l.starts_with(|c: char| c.is_ascii_digit() || c == '-')
    })
}

impl Updater {
    /// A new updater (qBittorrent's constructor): validates the settings and
    /// schedules the first check (immediately unless the previous one is
    /// less than 30 minutes old).
    pub fn new(prefs: &WebUiPrefs, last_ip: Option<IpAddr>, last_check: i64, now: i64, log: Log<'_>) -> Self {
        let mut u = Self {
            service: prefs.dyndns_service,
            domain: prefs.dyndns_domain.clone(),
            username: prefs.dyndns_username.clone(),
            password: prefs.dyndns_password.clone(),
            state: UpdaterState::Ok,
            timer_running: true,
            last_ip,
            last_check,
            next_check: None,
        };
        u.validate(log);
        if u.state == UpdaterState::Ok {
            u.next_check = Some(if last_check <= 0 || now - last_check > IP_CHECK_INTERVAL_SECS {
                now
            } else {
                last_check + IP_CHECK_INTERVAL_SECS
            });
        }
        u
    }

    fn validate(&mut self, log: Log<'_>) {
        let error = if !is_valid_domain(&self.domain) {
            Some("Dynamic DNS error: supplied domain name is invalid.")
        } else if self.username.chars().count() < 4 {
            Some("Dynamic DNS error: supplied username is too short.")
        } else if self.password.chars().count() < 4 {
            Some("Dynamic DNS error: supplied password is too short.")
        } else {
            None
        };
        if let Some(msg) = error {
            log(LogLevel::Critical, msg.to_string());
            self.last_ip = None;
            self.timer_running = false;
            self.next_check = None;
            self.state = UpdaterState::InvalidCredentials;
        }
    }

    /// Preferences changed (`updateCredentials`).
    pub fn update_credentials(&mut self, prefs: &WebUiPrefs, now: i64, log: Log<'_>) {
        if self.state == UpdaterState::Fatal {
            return;
        }
        let changed = self.service != prefs.dyndns_service
            || self.domain != prefs.dyndns_domain
            || self.username != prefs.dyndns_username
            || self.password != prefs.dyndns_password;
        if !changed {
            return;
        }
        self.service = prefs.dyndns_service;
        self.domain = prefs.dyndns_domain.clone();
        self.username = prefs.dyndns_username.clone();
        self.password = prefs.dyndns_password.clone();
        let was_invalid = self.state == UpdaterState::InvalidCredentials;
        self.state = UpdaterState::Ok;
        self.validate(log);
        if self.state == UpdaterState::Ok && was_invalid {
            // Try again right away.
            self.timer_running = true;
            self.next_check = Some(now);
        }
    }

    /// Handles the checkip page; returns the IP to report when it changed.
    pub fn process_ip_page(&mut self, body: &str, now: i64) -> Option<IpAddr> {
        self.last_check = now;
        let start = body.find("Current IP Address:")? + "Current IP Address:".len();
        let rest = &body[start..];
        let rest = rest.trim_start();
        let end = rest.find('<')?;
        if !rest[end..].starts_with("</body>") {
            return None;
        }
        let ip: IpAddr = rest[..end].trim().parse().ok()?;
        if self.last_ip == Some(ip) {
            return None;
        }
        self.last_ip = Some(ip);
        Some(ip)
    }

    /// Update URL for `ip` (credentials are sent separately).
    pub fn update_url(&self, ip: IpAddr) -> String {
        let host = match self.service {
            DynDnsService::DynDns => "members.dyndns.org",
            DynDnsService::NoIp => "dynupdate.no-ip.com",
        };
        let mut url = reqwest::Url::parse(&format!("https://{host}/nic/update")).expect("static URL");
        url.query_pairs_mut().append_pair("hostname", &self.domain).append_pair("myip", &ip.to_string());
        url.to_string()
    }

    /// Handles the update reply (first word = return code).
    pub fn process_update_reply(&mut self, reply: &str, log: Log<'_>) {
        let code = reply.split_whitespace().next().unwrap_or("");
        match code {
            "good" | "nochg" => {
                log(LogLevel::Info, "Your dynamic DNS was successfully updated.".to_string());
                return;
            }
            "911" | "dnserr" => {
                log(
                    LogLevel::Critical,
                    "Dynamic DNS error: The service is temporarily unavailable, it will be retried in 30 minutes."
                        .to_string(),
                );
                self.last_ip = None;
                return;
            }
            _ => {}
        }
        // Everything else stops updating until the user changes something.
        self.timer_running = false;
        self.next_check = None;
        self.last_ip = None;
        let (state, msg) = match code {
            "nohost" => (
                UpdaterState::InvalidCredentials,
                "Dynamic DNS error: hostname supplied does not exist under specified account.".to_string(),
            ),
            "badauth" => (UpdaterState::InvalidCredentials, "Dynamic DNS error: Invalid username/password.".to_string()),
            "badagent" => (
                UpdaterState::Fatal,
                "Dynamic DNS error: qBit NeoRS was blacklisted by the service.".to_string(),
            ),
            "!donator" => (UpdaterState::Fatal, "Dynamic DNS error: !donator was returned by the service.".to_string()),
            "abuse" => (UpdaterState::Fatal, "Dynamic DNS error: Your username was blocked due to abuse.".to_string()),
            _ => return,
        };
        log(LogLevel::Critical, msg);
        self.state = state;
    }
}

/// Persisted between runs (qBittorrent `DNSUpdater/lastIP`, `lastUpdateTime`).
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    last_ip: String,
    #[serde(default)]
    last_check: i64,
}

pub struct DynDnsUpdater {
    ctx: CoreContext,
    state_file: PathBuf,
    inner: Mutex<Option<Updater>>,
}

/// Starts the updater task; it follows preference changes for the lifetime
/// of the application (one task, idle while DynDNS is disabled).
pub fn launch(ctx: &CoreContext) -> Arc<DynDnsUpdater> {
    let this = Arc::new(DynDnsUpdater {
        ctx: ctx.clone(),
        state_file: ctx.profile.data_dir.join("dyndns.json"),
        inner: Mutex::new(None),
    });
    let task = this.clone();
    ctx.rt.spawn(async move { task.run().await });
    this
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl DynDnsUpdater {
    /// Current state (None while inactive).
    pub fn state(&self) -> Option<UpdaterState> {
        self.inner.lock().as_ref().map(|u| u.state)
    }

    fn logger(&self) -> impl FnMut(LogLevel, String) + '_ {
        |level, msg| self.ctx.logger.log(level, msg)
    }

    fn load_saved(&self) -> (Option<IpAddr>, i64) {
        let saved: Saved =
            std::fs::read(&self.state_file).ok().and_then(|d| serde_json::from_slice(&d).ok()).unwrap_or_default();
        (saved.last_ip.parse().ok(), saved.last_check)
    }

    fn save(&self) {
        let saved = {
            let g = self.inner.lock();
            let Some(u) = g.as_ref() else { return };
            Saved { last_ip: u.last_ip.map(|i| i.to_string()).unwrap_or_default(), last_check: u.last_check }
        };
        if let Ok(data) = serde_json::to_vec(&saved) {
            let _ = crate::utils::fs::write_atomic_mode(&self.state_file, &data, 0o600);
        }
    }

    /// Applies the current preferences; returns when the next check is due.
    fn reconfigure(&self, prefs: &WebUiPrefs) -> Option<i64> {
        let active = prefs.web_ui_enabled && prefs.dyndns_enabled;
        if !active {
            let was_active = self.inner.lock().is_some();
            if was_active {
                self.save();
                *self.inner.lock() = None;
            }
            return None;
        }
        let has = self.inner.lock().is_some();
        if !has {
            let (ip, last) = self.load_saved();
            let u = Updater::new(prefs, ip, last, now(), &mut self.logger());
            *self.inner.lock() = Some(u);
        } else {
            let mut log_lines = Vec::new();
            if let Some(u) = self.inner.lock().as_mut() {
                u.update_credentials(prefs, now(), &mut |l, m| log_lines.push((l, m)));
            }
            for (l, m) in log_lines {
                self.ctx.logger.log(l, m);
            }
        }
        self.inner.lock().as_ref().and_then(|u| if u.timer_running { u.next_check } else { None })
    }

    async fn run(self: Arc<Self>) {
        let mut prefs_rx = self.ctx.prefs.subscribe();
        let mut shutdown = self.ctx.events.shutdown_requested();
        loop {
            let prefs = prefs_rx.borrow_and_update().clone();
            let due = self.reconfigure(&prefs.webui);
            let sleep = match due {
                Some(t) => Duration::from_secs((t - now()).max(0) as u64),
                None => Duration::from_secs(365 * 24 * 3600),
            };
            tokio::select! {
                changed = prefs_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                _ = shutdown.changed() => break,
                _ = tokio::time::sleep(sleep), if due.is_some() => {
                    self.check_public_ip().await;
                }
            }
        }
        self.save();
    }

    async fn check_public_ip(&self) {
        {
            let mut g = self.inner.lock();
            let Some(u) = g.as_mut() else { return };
            u.last_check = now();
            u.next_check = Some(u.last_check + IP_CHECK_INTERVAL_SECS);
        }
        let opts = FetchOptions { user_agent: Some(http::app_user_agent()), basic_auth: None };
        let req = HttpRequest { url: CHECK_IP_URL.to_string(), cookie: None, max_size: 64 * 1024, timeout_secs: 60 };
        let body = match http::fetch_with(&self.ctx, req, HttpPurpose::General, opts.clone()).await {
            Ok(r) if (200..300).contains(&r.status) => String::from_utf8_lossy(&r.body).into_owned(),
            Ok(r) => {
                tracing::debug!("DynDNS IP request failed: HTTP {}", r.status);
                return;
            }
            Err(e) => {
                tracing::debug!("DynDNS IP request failed: {e}");
                return;
            }
        };
        let update = {
            let mut g = self.inner.lock();
            let Some(u) = g.as_mut() else { return };
            u.process_ip_page(&body, now()).map(|ip| (u.update_url(ip), u.username.clone(), u.password.clone()))
        };
        self.save();
        let Some((url, user, pass)) = update else { return };
        let opts = FetchOptions { basic_auth: Some((user, pass)), ..opts };
        let req = HttpRequest { url, cookie: None, max_size: 64 * 1024, timeout_secs: 60 };
        match http::fetch_with(&self.ctx, req, HttpPurpose::General, opts).await {
            Ok(r) if (200..300).contains(&r.status) => {
                let reply = String::from_utf8_lossy(&r.body).into_owned();
                let mut log_lines = Vec::new();
                if let Some(u) = self.inner.lock().as_mut() {
                    u.process_update_reply(&reply, &mut |l, m| log_lines.push((l, m)));
                }
                for (l, m) in log_lines {
                    self.ctx.logger.log(l, m);
                }
                self.save();
            }
            Ok(r) => tracing::debug!("DynDNS update failed: HTTP {}", r.status),
            Err(e) => tracing::debug!("DynDNS update failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs() -> WebUiPrefs {
        WebUiPrefs {
            web_ui_enabled: true,
            dyndns_enabled: true,
            dyndns_domain: "myhost.dyndns.org".into(),
            dyndns_username: "user".into(),
            dyndns_password: "secret".into(),
            ..Default::default()
        }
    }

    #[test]
    fn domain_validation() {
        assert!(is_valid_domain("myhost.dyndns.org"));
        assert!(is_valid_domain("a-b.example.co"));
        assert!(!is_valid_domain("changeme"));
        assert!(!is_valid_domain("1host.example.com"));
        assert!(!is_valid_domain("-host.example.com"));
        assert!(!is_valid_domain("host.example.c0m"));
        assert!(!is_valid_domain("host..com"));
        assert!(!is_valid_domain(&format!("{}.com", "a".repeat(64))));
    }

    #[test]
    fn schedule_and_validation() {
        let mut logs = Vec::new();
        let mut log = |l: LogLevel, m: String| logs.push((l, m));
        let u = Updater::new(&prefs(), None, 0, 10_000, &mut log);
        assert_eq!(u.state, UpdaterState::Ok);
        assert_eq!(u.next_check, Some(10_000));
        // A recent check is not repeated before 30 minutes.
        let u = Updater::new(&prefs(), None, 9_000, 10_000, &mut log);
        assert_eq!(u.next_check, Some(9_000 + IP_CHECK_INTERVAL_SECS));
        assert!(logs.is_empty());

        let mut p = prefs();
        p.dyndns_password = "abc".into();
        let mut logs = Vec::new();
        let mut u = Updater::new(&p, None, 0, 10_000, &mut |l, m| logs.push((l, m)));
        assert_eq!(u.state, UpdaterState::InvalidCredentials);
        assert_eq!(u.next_check, None);
        assert_eq!(logs, vec![(LogLevel::Critical, "Dynamic DNS error: supplied password is too short.".to_string())]);
        // Fixing the password retries immediately.
        u.update_credentials(&prefs(), 20_000, &mut |_, _| {});
        assert_eq!(u.state, UpdaterState::Ok);
        assert_eq!(u.next_check, Some(20_000));
        assert!(u.timer_running);
    }

    #[test]
    fn ip_page_and_update_url() {
        let mut u = Updater::new(&prefs(), None, 0, 1, &mut |_, _| {});
        let page = "<html><head><title>Current IP Check</title></head><body>Current IP Address: 203.0.113.7</body></html>";
        assert_eq!(u.process_ip_page(page, 5), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(u.last_check, 5);
        // Unchanged IP: nothing to report.
        assert_eq!(u.process_ip_page(page, 6), None);
        assert_eq!(u.process_ip_page("<body>nothing</body>", 7), None);
        assert_eq!(u.process_ip_page("Current IP Address: 999.1.1.1</body>", 7), None);
        assert_eq!(
            u.update_url("203.0.113.7".parse().unwrap()),
            "https://members.dyndns.org/nic/update?hostname=myhost.dyndns.org&myip=203.0.113.7"
        );
        u.service = DynDnsService::NoIp;
        u.domain = "a.b.com&x=y".into();
        assert_eq!(
            u.update_url("2001:db8::1".parse().unwrap()),
            "https://dynupdate.no-ip.com/nic/update?hostname=a.b.com%26x%3Dy&myip=2001%3Adb8%3A%3A1"
        );
    }

    #[test]
    fn update_replies() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        let fresh = || {
            let mut u = Updater::new(&prefs(), None, 0, 1, &mut |_, _| {});
            u.last_ip = Some(ip);
            u
        };
        let run = |reply: &str| {
            let mut u = fresh();
            let mut logs = Vec::new();
            u.process_update_reply(reply, &mut |l, m| logs.push((l, m)));
            (u, logs)
        };

        let (u, logs) = run("good 203.0.113.7\n");
        assert_eq!(u.state, UpdaterState::Ok);
        assert_eq!(u.last_ip, Some(ip));
        assert_eq!(logs[0], (LogLevel::Info, "Your dynamic DNS was successfully updated.".to_string()));
        let (u, _) = run("nochg 203.0.113.7");
        assert!(u.timer_running);

        let (u, logs) = run("911");
        assert_eq!(u.state, UpdaterState::Ok);
        assert!(u.timer_running);
        assert_eq!(u.last_ip, None, "retried at the next check");
        assert_eq!(logs[0].0, LogLevel::Critical);

        let (mut u, logs) = run("badauth");
        assert_eq!(u.state, UpdaterState::InvalidCredentials);
        assert!(!u.timer_running);
        assert_eq!(logs[0].1, "Dynamic DNS error: Invalid username/password.");
        // Changing the credentials resumes.
        let mut p = prefs();
        p.dyndns_password = "other-secret".into();
        u.update_credentials(&p, 50, &mut |_, _| {});
        assert_eq!(u.state, UpdaterState::Ok);
        assert_eq!(u.next_check, Some(50));

        let (u, _) = run("nohost");
        assert_eq!(u.state, UpdaterState::InvalidCredentials);

        for code in ["badagent", "!donator", "abuse"] {
            let (mut u, logs) = run(code);
            assert_eq!(u.state, UpdaterState::Fatal, "{code}");
            assert_eq!(logs.len(), 1);
            let mut p = prefs();
            p.dyndns_password = "other-secret".into();
            u.update_credentials(&p, 50, &mut |_, _| {});
            assert_eq!(u.state, UpdaterState::Fatal, "never retried");
            assert!(!u.timer_running);
        }

        // Unknown code: stop checking, no state change, no log.
        let (u, logs) = run("weird");
        assert_eq!(u.state, UpdaterState::Ok);
        assert!(!u.timer_running);
        assert!(logs.is_empty());
    }

    #[test]
    fn launch_follows_preferences() {
        let rt = crate::net::testutil::runtime();
        let dir = tempfile::tempdir().unwrap();
        let mut p = crate::prefs::Preferences { webui: prefs(), ..Default::default() };
        p.webui.dyndns_domain = "invalid".into();
        let ctx = crate::net::testutil::context_with(&rt, dir.path(), p);
        let d = launch(&ctx);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while d.state().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(d.state(), Some(UpdaterState::InvalidCredentials));
        ctx.prefs.update(|p| p.webui.dyndns_enabled = false).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while d.state().is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(d.state(), None);
        // Credentials are never logged.
        assert!(ctx.logger.messages(-1, &[]).iter().all(|m| !m.message.contains("secret")));
    }
}
