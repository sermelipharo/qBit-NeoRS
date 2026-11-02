//! End-to-end tests: a real `App` on a temporary profile with the WebUI on a
//! free local port, driven over HTTP.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::api::{ENDPOINTS, Methods};
use super::password::hash_password;
use crate::prefs::Preferences;
use crate::{App, AppConfig};

const PASSWORD: &str = "adminadmin";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Harness {
    app: Arc<App>,
    port: u16,
    client: reqwest::Client,
    _dir: tempfile::TempDir,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app.shutdown();
    }
}

fn hashed_password() -> String {
    // Hashing is slow in debug builds: do it once per test binary.
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| hash_password(PASSWORD)).clone()
}

impl Harness {
    fn new(tweak: impl FnOnce(&mut Preferences)) -> Self {
        Self::build(tweak, true)
    }

    fn build(tweak: impl FnOnce(&mut Preferences), expect_running: bool) -> Self {
        Self::build_with(|_, _| {}, tweak, expect_running)
    }

    /// `prepare` edits the profile folder and the stored preferences before
    /// the application starts.
    fn build_with(
        prepare: impl FnOnce(&std::path::Path, &mut Preferences),
        tweak: impl FnOnce(&mut Preferences),
        expect_running: bool,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        // No DHT/LSD/UPnP: tests stay local and server_state stays stable.
        let mut initial = Preferences::default();
        initial.bittorrent.dht = false;
        initial.bittorrent.lsd = false;
        initial.connection.upnp = false;
        initial.connection.listen_port = 0;
        prepare(dir.path(), &mut initial);
        std::fs::create_dir_all(dir.path().join("config")).unwrap();
        std::fs::write(dir.path().join("config/preferences.json"), serde_json::to_vec(&initial).unwrap()).unwrap();
        let app = App::new(AppConfig { profile_dir: dir.path().to_string_lossy().into_owned(), ..Default::default() }).unwrap();
        let mut p = app.preferences();
        p.webui.web_ui_enabled = true;
        p.webui.web_ui_address = "127.0.0.1".into();
        p.webui.web_ui_password_pbkdf2 = hashed_password();
        p.behavior.file_log_enabled = false;
        tweak(&mut p);
        // The WebUI takes a port number (0 means "default"), so a free port
        // is probed first. Another test or an outgoing connection can take it
        // before the bind: only that failure is retried, on a new port.
        let mut attempt = 0;
        let port = loop {
            attempt += 1;
            let port = free_port();
            p.webui.web_ui_port = i64::from(port);
            app.set_preferences(p.clone()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(20);
            let s = loop {
                let s = app.webui_status();
                if (s.port == port && (s.running || !s.error.is_empty())) || Instant::now() > deadline {
                    break s;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            if !s.running && s.error.starts_with("Unable to bind") && attempt < 20 {
                continue;
            }
            if expect_running {
                assert!(s.running, "WebUI did not start: {s:?}");
            }
            break port;
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Self { app, port, client, _dir: dir }
    }

    fn block<F: std::future::Future>(&self, f: F) -> F::Output {
        self.app.runtime().block_on(f)
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    async fn get(&self, path: &str, cookie: Option<&str>) -> reqwest::Response {
        let mut rb = self.client.get(self.url(path));
        if let Some(c) = cookie {
            rb = rb.header("Cookie", c);
        }
        rb.send().await.unwrap()
    }

    async fn post(&self, path: &str, form: &[(&str, &str)], cookie: Option<&str>) -> reqwest::Response {
        let mut rb = self.client.post(self.url(path)).form(form);
        if let Some(c) = cookie {
            rb = rb.header("Cookie", c);
        }
        rb.send().await.unwrap()
    }

    /// Logs in and returns the `Cookie` header value.
    async fn login(&self) -> String {
        let r = self.post("/api/v2/auth/login", &[("username", "admin"), ("password", PASSWORD)], None).await;
        assert_eq!(r.status(), 204);
        let set = r.headers().get("set-cookie").expect("cookie").to_str().unwrap().to_owned();
        set.split(';').next().unwrap().to_owned()
    }

    async fn raw(&self, request: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await;
        String::from_utf8_lossy(&buf).into_owned()
    }
}

fn status_line(resp: &str) -> &str {
    resp.lines().next().unwrap_or("")
}

#[test]
fn login_logout_and_cookie_attributes() {
    let h = Harness::new(|_| {});
    h.block(async {
        let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", "wrong")], None).await;
        assert_eq!(r.status(), 401);
        let r = h.post("/api/v2/auth/login", &[("username", "nobody"), ("password", PASSWORD)], None).await;
        assert_eq!(r.status(), 401);

        let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", PASSWORD)], None).await;
        assert_eq!(r.status(), 204);
        let set = r.headers().get("set-cookie").unwrap().to_str().unwrap().to_owned();
        assert!(set.starts_with(&format!("QBT_SID_{}=", h.port)), "{set}");
        assert!(set.contains("HttpOnly"));
        assert!(set.contains("SameSite=Strict"));
        assert!(set.contains("Path=/"));
        assert!(!set.contains("Secure"), "no Secure flag over plain HTTP");
        let cookie = set.split(';').next().unwrap().to_owned();
        let sid = cookie.split_once('=').unwrap().1.to_owned();
        assert_eq!(sid.len(), 32);

        // Second login with a session is a no-op 204 without a new cookie.
        let r = h.post("/api/v2/auth/login", &[], Some(&cookie)).await;
        assert_eq!(r.status(), 204);
        assert!(r.headers().get("set-cookie").is_none());

        assert_eq!(h.get("/api/v2/app/version", Some(&cookie)).await.status(), 200);
        // Logout invalidates the session server-side.
        let r = h.post("/api/v2/auth/logout", &[], Some(&cookie)).await;
        assert_eq!(r.status(), 200);
        assert!(r.headers().get("set-cookie").unwrap().to_str().unwrap().contains("Expires="));
        assert_eq!(h.get("/api/v2/app/version", Some(&cookie)).await.status(), 403);
        // A forged session ID is never accepted.
        let forged = format!("QBT_SID_{}=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", h.port);
        assert_eq!(h.get("/api/v2/app/version", Some(&forged)).await.status(), 403);
        // Logout without a session is forbidden.
        assert_eq!(h.post("/api/v2/auth/logout", &[], None).await.status(), 403);
    });
}

#[test]
fn version_endpoints() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let r = h.get("/api/v2/app/version", Some(&c)).await;
        assert_eq!(r.headers()["content-type"], "text/plain; charset=UTF-8");
        assert_eq!(r.text().await.unwrap(), "v5.2.4");
        assert_eq!(h.get("/api/v2/app/webapiVersion", Some(&c)).await.text().await.unwrap(), "2.15.1");
        let info: Value = h.get("/api/v2/app/buildInfo", Some(&c)).await.json().await.unwrap();
        assert_eq!(info["bitness"], 64);
        let p: Value = h.get("/api/v2/app/processInfo", Some(&c)).await.json().await.unwrap();
        assert!(p["launch_time"].as_i64().unwrap() > 1_600_000_000);
        // Unknown action in a known scope.
        let r = h.get("/api/v2/app/nope", Some(&c)).await;
        assert_eq!(r.status(), 404);
        assert_eq!(r.text().await.unwrap(), "Endpoint does not exist");
        assert_eq!(h.get("/api/v2/nope/x", Some(&c)).await.status(), 404);
    });
}

#[test]
fn every_endpoint_requires_authentication() {
    let h = Harness::new(|_| {});
    h.block(async {
        for (scope, action, methods) in ENDPOINTS {
            if (*scope, *action) == ("auth", "login") {
                continue;
            }
            let path = format!("/api/v2/{scope}/{action}");
            let r = match methods {
                Methods::PostOnly => h.post(&path, &[("hashes", "all")], None).await,
                Methods::GetPost => h.get(&path, None).await,
            };
            assert_eq!(r.status(), 403, "{path} without a session");
            assert_eq!(r.headers()["x-content-type-options"], "nosniff", "{path}");
            assert!(r.headers().get("content-security-policy").is_some(), "{path}");
        }
        // Unknown endpoints do not reveal anything either.
        assert_eq!(h.get("/api/v2/app/doesNotExist", None).await.status(), 403);
        // The private UI is not served without a session.
        let r = h.get("/app.js", None).await;
        assert_eq!(r.status(), 404);
    });
}

#[test]
fn every_endpoint_exists_and_method_filter() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        for (scope, action, methods) in ENDPOINTS {
            if *scope == "auth" || (*scope, *action) == ("app", "shutdown") {
                continue;
            }
            let path = format!("/api/v2/{scope}/{action}");
            let r = h.post(&path, &[], Some(&c)).await;
            let status = r.status();
            let body = r.text().await.unwrap();
            assert_ne!(body, "Endpoint does not exist", "{path}");
            assert_ne!(status, 405, "{path}");
            assert_ne!(status, 403, "{path}");
            assert_ne!(status.as_u16(), 500, "{path}: {body}");
            let r = h.get(&path, Some(&c)).await;
            if *methods == Methods::PostOnly {
                assert_eq!(r.status(), 405, "GET {path}");
            } else {
                assert_ne!(r.status(), 405, "GET {path}");
            }
        }
        // Required parameters are reported.
        let r = h.post("/api/v2/torrents/delete", &[("hashes", "x")], Some(&c)).await;
        assert_eq!(r.status(), 400);
        assert_eq!(r.text().await.unwrap(), "Missing required parameters: deleteFiles");
    });
}

#[test]
fn preferences_round_trip() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let r = h.get("/api/v2/app/preferences", Some(&c)).await;
        assert_eq!(r.headers()["content-type"], "application/json");
        let text = r.text().await.unwrap();
        assert!(text.starts_with("{\"locale\":"));
        let prefs: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(prefs.as_object().unwrap().len(), 238);
        // qBittorrent 5.3 (WebAPI 2.16.1): search job storage, after confirm_torrent_deletion.
        let keys: Vec<&str> = prefs.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys[3..6], ["confirm_torrent_deletion", "store_search_jobs", "store_search_job_results"]);
        assert_eq!((prefs["store_search_jobs"].clone(), prefs["store_search_job_results"].clone()), (json!(false), json!(false)));
        assert_eq!(prefs["web_ui_port"], h.port);
        assert!(prefs.get("web_ui_password").is_none());

        // Partial update.
        let update = json!({"dht": false, "max_active_downloads": 7, "max_ratio_enabled": false, "max_ratio": 3}).to_string();
        let r = h.post("/api/v2/app/setPreferences", &[("json", &update)], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let after: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(after["dht"], false);
        assert_eq!(after["max_active_downloads"], 7);
        assert_eq!(after["max_ratio"], -1.0);
        assert_eq!(after["pex"], prefs["pex"]);

        // Sending everything back is the identity.
        let r = h.post("/api/v2/app/setPreferences", &[("json", &after.to_string())], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let again: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(again, after);

        // Invalid JSON is a no-op; missing parameter and bad values are 400.
        assert_eq!(h.post("/api/v2/app/setPreferences", &[("json", "{nope")], Some(&c)).await.status(), 200);
        assert_eq!(h.post("/api/v2/app/setPreferences", &[], Some(&c)).await.status(), 400);
        let r = h.post("/api/v2/app/setPreferences", &[("json", r#"{"web_ui_username":"ab"}"#)], Some(&c)).await;
        assert_eq!(r.status(), 400);
        assert_eq!(r.text().await.unwrap(), "WebUI username must be at least 3 characters long");
        let r = h.post("/api/v2/app/setPreferences", &[("json", r#"{"web_ui_password":"123"}"#)], Some(&c)).await;
        assert_eq!(r.text().await.unwrap(), "WebUI password must be at least 6 characters long");

        // A new password takes effect at once and logs every session out
        // (also sessions opened with the old credentials elsewhere).
        let other = h.login().await;
        let r = h.post("/api/v2/app/setPreferences", &[("json", r#"{"web_ui_password":"n3w-secret"}"#)], Some(&c)).await;
        assert_eq!(r.status(), 200);
        assert_eq!(h.get("/api/v2/app/version", Some(&c)).await.status(), 403);
        assert_eq!(h.get("/api/v2/app/version", Some(&other)).await.status(), 403);
        let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", "n3w-secret")], None).await;
        assert_eq!(r.status(), 204);
    });
}

#[test]
fn speed_limits_of_both_modes_in_one_call() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let get = || async { h.get("/api/v2/transfer/getSpeedLimits", Some(&c)).await.json::<Value>().await.unwrap() };
        let v = get().await;
        assert_eq!(v["alt_up_limit"], 10240, "{v}");
        let partial = h.post("/api/v2/transfer/setSpeedLimits", &[("up_limit", "2048")], Some(&c)).await;
        assert_eq!(partial.status(), 400, "all four limits are required");
        let form = [("up_limit", "3000"), ("dl_limit", "-5"), ("alt_up_limit", "500"), ("alt_dl_limit", "nope")];
        assert_eq!(h.post("/api/v2/transfer/setSpeedLimits", &form, Some(&c)).await.status(), 200);
        assert_eq!(get().await, json!({"up_limit": 2048, "dl_limit": 0, "alt_up_limit": 1024, "alt_dl_limit": 0}));
        // The single-mode endpoints see the same values.
        assert_eq!(h.get("/api/v2/transfer/uploadLimit", Some(&c)).await.text().await.unwrap(), "2048");
        assert_eq!(h.get("/api/v2/transfer/getSpeedLimits", None).await.status(), 403);
    });
}

#[test]
fn transfer_info_and_maindata_rid_flow() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let info: Value = h.get("/api/v2/transfer/info", Some(&c)).await.json().await.unwrap();
        let keys: Vec<&str> = info.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "dl_info_speed", "dl_info_data", "up_info_speed", "up_info_data", "dl_rate_limit", "up_rate_limit",
                "dht_nodes", "connection_status", "last_external_address_v4", "last_external_address_v6"
            ]
        );
        assert_eq!(h.get("/api/v2/transfer/speedLimitsMode", Some(&c)).await.text().await.unwrap(), "0");

        let r1: Value = h.get("/api/v2/sync/maindata", Some(&c)).await.json().await.unwrap();
        assert_eq!(r1["rid"], 1);
        assert_eq!(r1["full_update"], true);
        assert_eq!(r1["server_state"]["refresh_interval"], 1500);
        assert!(r1["server_state"].get("use_subcategories").is_none());
        let r2: Value = h.get("/api/v2/sync/maindata?rid=1", Some(&c)).await.json().await.unwrap();
        assert_eq!(r2["rid"], 2);
        assert!(r2.get("full_update").is_none());
        assert!(r2.get("server_state").is_none(), "{r2}");
        // A preference change shows up as a field-level server_state diff.
        let r = h.post("/api/v2/app/setPreferences", &[("json", r#"{"queueing_enabled":true}"#)], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let r3: Value = h.get("/api/v2/sync/maindata?rid=2", Some(&c)).await.json().await.unwrap();
        assert_eq!(r3["rid"], 3);
        assert_eq!(r3["server_state"], json!({"queueing": true}));
        // Unknown rid: full update.
        let r4: Value = h.get("/api/v2/sync/maindata?rid=999", Some(&c)).await.json().await.unwrap();
        assert_eq!(r4["full_update"], true);
        // Sessions do not share sync state.
        let other = h.login().await;
        let o: Value = h.get("/api/v2/sync/maindata?rid=3", Some(&other)).await.json().await.unwrap();
        assert_eq!(o["full_update"], true);
        assert_eq!(o["rid"], 1);
    });
}

#[test]
fn static_files() {
    let h = Harness::new(|_| {});
    h.block(async {
        let r = h.get("/", None).await;
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/html; charset=UTF-8");
        assert_eq!(r.headers()["cache-control"], "no-store");
        assert_eq!(r.headers()["x-frame-options"], "SAMEORIGIN");
        let csp = r.headers()["content-security-policy"].to_str().unwrap().to_owned();
        assert!(csp.contains("script-src 'self';") && !csp.contains("unsafe-inline"), "{csp}");
        assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
        let login_page = r.text().await.unwrap();
        assert!(!login_page.contains("QBT_TR("));
        assert!(!login_page.contains("${LANG}"));

        let c = h.login().await;
        let main = h.get("/", Some(&c)).await.text().await.unwrap();
        assert_ne!(main, login_page);
        assert!(main.contains("app.js?v="));
        let r = h.get("/app.js", Some(&c)).await;
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/javascript; charset=UTF-8");
        let r = h.get("/app.css", Some(&c)).await;
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "text/css; charset=UTF-8");
        assert_eq!(r.headers()["cache-control"], "private, max-age=43200");
        assert_eq!(h.get("/missing.html", Some(&c)).await.status(), 404);

        // Gzip is negotiated (and transparently decoded by the client).
        let r = h.client.get(h.url("/login.js")).header("Accept-Encoding", "gzip").send().await.unwrap();
        assert_eq!(r.status(), 200);

        // Traversal attempts, including encoded ones, are rejected.
        for path in ["/%2e%2e/%2e%2e/etc/passwd", "/..%2f..%2fetc%2fpasswd", "/a%5c..%5cb", "/./index.html", "/x/%2E%2E/index.html"] {
            let resp = h.raw(&format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", h.port)).await;
            assert!(status_line(&resp).contains(" 400 "), "{path}: {}", status_line(&resp));
        }
        // HEAD: headers only.
        let r = h.client.head(h.url("/")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.text().await.unwrap().is_empty());
    });
}

#[test]
fn transport_limits_and_methods() {
    let h = Harness::new(|_| {});
    h.block(async {
        let resp = h.raw(&format!("PUT / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", h.port)).await;
        assert!(status_line(&resp).contains(" 501 "), "{resp}");
        let c = h.login().await;
        let resp = h
            .raw(&format!(
                "POST /api/v2/torrents/add HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {c}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\nurls=x",
                h.port
            ))
            .await;
        assert!(status_line(&resp).contains(" 413 "), "{resp}");
        // An unauthenticated client cannot even make the server read a body.
        let resp = h
            .raw(&format!(
                "POST /api/v2/torrents/add HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Length: 999999999\r\nConnection: close\r\n\r\n",
                h.port
            ))
            .await;
        assert!(status_line(&resp).contains(" 403 "), "{resp}");
        // HEAD never runs an API action (it would bypass the POST-only
        // filter for GET endpoints); static files still answer it.
        let r = h.client.head(h.url("/api/v2/app/version")).header("Cookie", &c).send().await.unwrap();
        assert_eq!(r.status(), 405);
        let r = h.client.head(h.url("/")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        // Actions that add torrents or change settings are POST-only, so a
        // cross-site GET (image tag) cannot trigger them.
        for path in ["/api/v2/search/downloadTorrent", "/api/v2/rss/setFeedRefreshInterval"] {
            assert_eq!(h.get(path, Some(&c)).await.status(), 405, "{path}");
        }
        // Unknown body type.
        let r = h.client.post(h.url("/api/v2/app/setPreferences")).header("Cookie", &c).header("Content-Type", "text/plain").body("json={}").send().await.unwrap();
        assert_eq!(r.status(), 400);
        // Multipart parameters work (torrents/add with nothing usable -> 409).
        let form = reqwest::multipart::Form::new().text("urls", "").text("savepath", "/tmp");
        let r = h.client.post(h.url("/api/v2/torrents/add")).header("Cookie", &c).multipart(form).send().await.unwrap();
        assert_eq!(r.status(), 409);
        // A trickled login body is cut off after 10 s (not 120 s).
        let started = std::time::Instant::now();
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
        s.write_all(
            format!(
                "POST /api/v2/auth/login HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 100\r\nConnection: close\r\n\r\nusername=",
                h.port
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut buf = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut buf)).await;
        let resp = String::from_utf8_lossy(&buf);
        assert!(status_line(&resp).contains(" 408 "), "{resp}");
        assert!(started.elapsed() < Duration::from_secs(15));
        // Slow clients are disconnected (head read timeout).
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
        s.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(15), s.read(&mut buf)).await.expect("server closes slow client");
        assert!(matches!(n, Ok(0) | Ok(_) | Err(_)));
    });
}

#[test]
fn csrf_and_host_validation() {
    let h = Harness::new(|p| p.webui.web_ui_domain_list = "qbt.example".into());
    h.block(async {
        let host = format!("127.0.0.1:{}", h.port);
        // Valid: Host is the local address.
        let ok = h.raw(&format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")).await;
        assert!(status_line(&ok).contains(" 200 "));
        // Whitelisted domain.
        let ok = h.raw(&format!("GET / HTTP/1.1\r\nHost: QBT.example:{}\r\nConnection: close\r\n\r\n", h.port)).await;
        assert!(status_line(&ok).contains(" 200 "), "{ok}");
        // DNS rebinding style hosts are refused.
        for bad in ["evil.com", "qbt.example.evil.com", &format!("127.0.0.1:{}", h.port + 1)] {
            let r = h.raw(&format!("GET / HTTP/1.1\r\nHost: {bad}\r\nConnection: close\r\n\r\n")).await;
            assert!(status_line(&r).contains(" 401 "), "{bad}: {r}");
            assert!(r.contains("x-content-type-options: nosniff"));
        }
        // Cross-site Origin / Referer.
        let r = h.client.post(h.url("/api/v2/auth/login")).header("Origin", "http://evil.com").form(&[("username", "admin"), ("password", PASSWORD)]).send().await.unwrap();
        assert_eq!(r.status(), 401);
        // A link from another site to the start page works; anything else from there does not.
        let r = h.client.get(h.url("/")).header("Referer", "http://evil.com/x").send().await.unwrap();
        assert_eq!(r.status(), 200);
        let r = h.client.get(h.url("/")).header("Sec-Fetch-Site", "cross-site").send().await.unwrap();
        assert_eq!(r.status(), 200);
        let r = h.client.post(h.url("/")).header("Sec-Fetch-Site", "cross-site").send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = h.client.get(h.url("/api/v2/app/version")).header("Referer", "http://evil.com/x").send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = h.client.get(h.url("/scripts/x.js")).header("Sec-Fetch-Site", "cross-site").send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = h.client.get(h.url("/")).header("Origin", format!("http://{host}")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        // A page that suppresses Origin and Referer: the browser still says cross-site.
        let r = h.client.post(h.url("/api/v2/auth/login")).header("Sec-Fetch-Site", "cross-site").form(&[("username", "admin"), ("password", PASSWORD)]).send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = h.client.get(h.url("/")).header("Sec-Fetch-Site", "same-origin").send().await.unwrap();
        assert_eq!(r.status(), 200);
    });
}

#[test]
fn api_key_authentication() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let r: Value = h.post("/api/v2/app/rotateAPIKey", &[], Some(&c)).await.json().await.unwrap();
        let key = r["apiKey"].as_str().unwrap().to_owned();
        assert!(key.starts_with("qbt_") && key.len() == 32);
        let bearer = |k: &str| format!("Bearer {k}");
        let r = h.client.get(h.url("/api/v2/app/version")).header("Authorization", bearer(&key)).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.headers().get("set-cookie").is_none());
        // CSRF checks are skipped for API keys (non-browser clients).
        let r = h.client.get(h.url("/api/v2/app/version")).header("Authorization", bearer(&key)).header("Origin", "http://other").send().await.unwrap();
        assert_eq!(r.status(), 200);
        let r = h.client.get(h.url("/")).header("Authorization", bearer(&key)).send().await.unwrap();
        assert_eq!(r.status(), 404);
        let r = h.client.post(h.url("/api/v2/auth/logout")).header("Authorization", bearer(&key)).send().await.unwrap();
        assert_eq!(r.status(), 403);
        // Differs from the key in the last character (which could itself be 'x').
        let wrong = format!("{}{}", &key[..31], if key.ends_with('x') { 'y' } else { 'x' });
        let r = h.client.get(h.url("/api/v2/app/version")).header("Authorization", bearer(&wrong)).send().await.unwrap();
        assert_eq!(r.status(), 403);
        // The key cannot be used as a session cookie.
        let r = h.get("/api/v2/app/version", Some(&format!("QBT_SID_{}={key}", h.port))).await;
        assert_eq!(r.status(), 403);
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(prefs["web_ui_api_key"], key.as_str());
        // Rotation and deletion revoke the key immediately.
        assert_eq!(h.post("/api/v2/app/deleteAPIKey", &[], Some(&c)).await.status(), 204);
        let r = h.client.get(h.url("/api/v2/app/version")).header("Authorization", bearer(&key)).send().await.unwrap();
        assert_eq!(r.status(), 403);
    });
}

#[test]
fn brute_force_ban() {
    let h = Harness::new(|p| {
        p.webui.web_ui_max_auth_fail_count = 2;
        p.webui.web_ui_ban_duration = 3600;
    });
    h.block(async {
        for _ in 0..2 {
            let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", "bad")], None).await;
            assert_eq!(r.status(), 401);
        }
        let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", PASSWORD)], None).await;
        assert_eq!(r.status(), 403);
        assert_eq!(r.text().await.unwrap(), "Your IP address has been banned after too many failed authentication attempts.");
        // Forwarded headers from an untrusted peer do not change the client identity.
        let r = h
            .client
            .post(h.url("/api/v2/auth/login"))
            .header("X-Forwarded-For", "203.0.113.9")
            .form(&[("username", "admin"), ("password", PASSWORD)])
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        // Basic auth is subject to the same ban.
        let r = h.client.get(h.url("/api/v2/app/version")).basic_auth("admin", Some(PASSWORD)).send().await.unwrap();
        assert_eq!(r.status(), 403);
    });
}

#[test]
fn concurrent_logins_cannot_outrun_the_ban() {
    let h = Harness::new(|p| {
        p.webui.web_ui_max_auth_fail_count = 2;
        p.webui.web_ui_ban_duration = 3600;
    });
    h.block(async {
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..12 {
            let rb = h.client.post(h.url("/api/v2/auth/login")).form(&[("username", "admin"), ("password", "bad")]);
            set.spawn(async move { rb.send().await.unwrap().status().as_u16() });
        }
        let statuses: Vec<u16> = set.join_all().await;
        // Only `max_auth_fail` guesses are verified; the rest are refused as banned.
        assert_eq!(statuses.iter().filter(|s| **s == 401).count(), 2, "{statuses:?}");
        assert_eq!(statuses.iter().filter(|s| **s == 403).count(), 10, "{statuses:?}");
    });
}

#[test]
fn basic_auth_and_local_bypass() {
    let h = Harness::new(|_| {});
    h.block(async {
        let r = h.client.get(h.url("/api/v2/app/version")).basic_auth("admin", Some(PASSWORD)).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.headers().get("set-cookie").is_some());
        let r = h.client.get(h.url("/api/v2/app/version")).basic_auth("admin", Some("nope")).send().await.unwrap();
        assert_eq!(r.status(), 401);
        assert_eq!(h.get("/api/v2/app/version", None).await.status(), 403);
    });
    let h = Harness::new(|p| p.webui.bypass_local_auth = true);
    h.block(async {
        let r = h.get("/api/v2/app/version", None).await;
        assert_eq!(r.status(), 200);
        assert!(r.headers().get("set-cookie").is_some());
        // DNS rebinding: a page on evil.example rebound to 127.0.0.1 sends
        // same-origin requests with its own Host; the bypass must not apply.
        let port = h.port;
        let get = |host: String| format!("GET /api/v2/app/version HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        let r = h.raw(&get(format!("evil.example:{port}"))).await;
        assert_eq!(status_line(&r), "HTTP/1.1 403 Forbidden", "{r}");
        let r = h
            .raw(&format!(
                "POST /api/v2/app/setPreferences HTTP/1.1\r\nHost: evil.example:{port}\r\nOrigin: http://evil.example:{port}\r\n\
                 Content-Type: application/x-www-form-urlencoded\r\nContent-Length: 7\r\nConnection: close\r\n\r\njson={{}}"
            ))
            .await;
        assert_eq!(status_line(&r), "HTTP/1.1 403 Forbidden", "{r}");
        let r = h.raw(&get(format!("localhost:{port}"))).await;
        assert_eq!(status_line(&r), "HTTP/1.1 200 OK", "{r}");
        // A reverse proxy on the same host connects from loopback; requests it
        // forwards (any forwarding header, even a loopback X-Forwarded-For
        // passed through from the client) must not get the localhost bypass.
        for fwd in ["X-Forwarded-For: 127.0.0.1", "X-Real-IP: 203.0.113.7", "Forwarded: for=203.0.113.7", "Via: 1.1 proxy"] {
            let r = h
                .raw(&format!("GET /api/v2/app/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{fwd}\r\nConnection: close\r\n\r\n"))
                .await;
            assert_eq!(status_line(&r), "HTTP/1.1 403 Forbidden", "{fwd}: {r}");
        }
    });
}

#[test]
fn neors_name_rules() {
    use crate::model::{TorrentNameRule, TorrentNameRules};
    let h = Harness::new(|_| {});
    h.block(async {
        assert_eq!(h.get("/api/v2/neors/nameRules", None).await.status(), 403);
        let c = h.login().await;
        // Nothing pushed yet (the daemon): no rules.
        let v: Value = h.get("/api/v2/neors/nameRules", Some(&c)).await.json().await.unwrap();
        assert_eq!(v["available"], false);
        assert_eq!(v["rules"], json!([]));

        h.app.set_torrent_name_rules(TorrentNameRules {
            rules: vec![TorrentNameRule {
                pattern: r"\d{3,4}p".into(),
                category: "resolution".into(),
                template: String::new(),
                case_sensitive: false,
            }],
            enabled: true,
            semibold_titles: false,
            card_tags: true,
            year_as_tag: false,
        });
        let r = h.get("/api/v2/neors/nameRules", Some(&c)).await;
        assert_eq!(r.headers()["content-type"], "application/json");
        let v: Value = r.json().await.unwrap();
        let version = v["version"].as_str().unwrap().to_owned();
        assert_eq!(version.len(), 16);
        assert_eq!(
            v,
            json!({
                "version": version,
                "available": true,
                "options": {"enabled": true, "semiboldTitles": false, "cardTags": true, "yearAsTag": false},
                "rules": [{"pattern": r"\d{3,4}p", "category": "resolution", "template": "", "caseSensitive": false}],
            })
        );
        // Unchanged since the version the client has: only the version.
        let v: Value = h.get(&format!("/api/v2/neors/nameRules?since={version}"), Some(&c)).await.json().await.unwrap();
        assert_eq!(v, json!({"version": version, "unchanged": true}));
        let v: Value = h.get("/api/v2/neors/nameRules?since=0", Some(&c)).await.json().await.unwrap();
        assert_eq!(v["rules"].as_array().unwrap().len(), 1);
        // POST works too (like every GET endpoint); other actions do not exist.
        assert_eq!(h.post("/api/v2/neors/nameRules", &[], Some(&c)).await.status(), 200);
        assert_eq!(h.get("/api/v2/neors/other", Some(&c)).await.status(), 404);
    });
}

#[test]
fn clientdata_persistence() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let r = h.post("/api/v2/clientdata/store", &[("data", r#"{"a":1,"b":[true]}"#)], Some(&c)).await;
        assert_eq!(r.status(), 204);
        let v: Value = h.get("/api/v2/clientdata/load?keys=%5B%22a%22%5D", Some(&c)).await.json().await.unwrap();
        assert_eq!(v, json!({"a": 1}));
        let r = h.post("/api/v2/clientdata/store", &[("data", "[1]")], Some(&c)).await;
        assert_eq!(r.text().await.unwrap(), "`data` must be an object");
        let r = h.get("/api/v2/clientdata/load?keys=%5B1%5D", Some(&c)).await;
        assert_eq!(r.text().await.unwrap(), "Items of `keys` must be strings");
    });
    assert!(h.app.context().profile.config_dir.join("webui_clientdata.json").exists());
}

#[test]
fn https_serves_tls_and_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, include_str!("testdata/cert.pem")).unwrap();
    std::fs::write(&key, include_str!("testdata/key.pem")).unwrap();
    let (c, k) = (cert.to_string_lossy().into_owned(), key.to_string_lossy().into_owned());
    let h = Harness::new(move |p| {
        p.webui.use_https = true;
        p.webui.web_ui_https_cert_path = c;
        p.webui.web_ui_https_key_path = k;
    });
    assert!(h.app.webui_status().url.starts_with("https://"));
    h.block(async {
        let url = format!("https://127.0.0.1:{}/api/v2/auth/login", h.port);
        let r = h.client.post(&url).form(&[("username", "admin"), ("password", PASSWORD)]).send().await.unwrap();
        assert_eq!(r.status(), 204);
        let set = r.headers()["set-cookie"].to_str().unwrap();
        assert!(set.contains("; Secure"), "{set}");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("upgrade-insecure-requests"));
        // Plain HTTP is not served on an HTTPS port.
        assert!(h.client.get(h.url("/")).send().await.map(|r| r.status() != 200).unwrap_or(true));
    });

    // Invalid key: the server must not start (no fallback to HTTP).
    std::fs::write(&key, "garbage").unwrap();
    let (c, k) = (cert.to_string_lossy().into_owned(), key.to_string_lossy().into_owned());
    let h = Harness::build(
        move |p| {
            p.webui.use_https = true;
            p.webui.web_ui_https_cert_path = c;
            p.webui.web_ui_https_key_path = k;
        },
        false,
    );
    let s = h.app.webui_status();
    assert!(!s.running);
    assert!(s.error.starts_with("HTTPS setup failed"), "{s:?}");
    // No plain-HTTP fallback was started. The port itself cannot be probed:
    // another test may have bound it since, so the server state is checked.
    assert!(!h.app.webui_status().running);
}

#[test]
fn temporary_password_when_none_is_set() {
    let h = Harness::new(|p| p.webui.web_ui_password_pbkdf2.clear());
    let s = h.app.webui_status();
    assert_eq!(s.temporary_password.len(), 9);
    h.block(async {
        let r = h.post("/api/v2/auth/login", &[("username", "admin"), ("password", s.temporary_password.as_str())], None).await;
        assert_eq!(r.status(), 204);
    });
    // Setting a real password discards it.
    let mut p = h.app.preferences();
    p.webui.web_ui_password_pbkdf2 = hashed_password();
    h.app.set_preferences(p).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.app.webui_status().temporary_password.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(h.app.webui_status().temporary_password.is_empty());
}

/// `torrents/add` reads the same options from a multipart body (file parts
/// and text fields, as the WebUI and FlexGet send them) as from a
/// urlencoded one, including qBittorrent 4.x's `paused`/`root_folder`.
#[test]
fn add_options_in_multipart_and_urlencoded_bodies() {
    let h = Harness::new(|_| {});
    let base = h._dir.path().to_string_lossy().into_owned();
    h.block(async {
        let c = h.login().await;
        let info = |hash: String| {
            let (h, c) = (&h, c.clone());
            async move {
                let r = h.get(&format!("/api/v2/torrents/info?hashes={hash}"), Some(&c)).await;
                let v: serde_json::Value = r.json().await.unwrap();
                v.as_array().and_then(|a| a.first()).cloned().unwrap_or_default()
            }
        };

        // File + fields.
        let tor = crate::migrate::libtorrent::tests::sample_torrent(16, &[7u8; 40], b"nfo!");
        let id = crate::bittorrent::metainfo::parse_torrent(&tor, Default::default()).unwrap().meta.id();
        let save = format!("{base}/custom");
        let form = reqwest::multipart::Form::new()
            .part("torrents", reqwest::multipart::Part::bytes(tor.clone()).file_name("a.torrent"))
            .text("savepath", save.clone())
            .text("category", "Cat")
            .text("tags", "t1,t2")
            .text("contentLayout", "NoSubfolder")
            .text("stopped", "true")
            .text("autoTMM", "false")
            .text("upLimit", "4096");
        let r = h.client.post(h.url("/api/v2/torrents/add")).header("Cookie", &c).multipart(form).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let t = info(id).await;
        assert_eq!(t["save_path"], serde_json::json!(save));
        assert_eq!(t["category"], "Cat");
        assert_eq!(t["tags"], "t1, t2");
        assert_eq!(t["auto_tmm"], false);
        assert_eq!(t["up_limit"], 4096);
        assert!(t["state"].as_str().unwrap().starts_with("stopped"), "{t}");
        assert_eq!(t["content_path"], serde_json::json!(save), "NoSubfolder: content is the save path");

        // Magnet URL + fields in multipart, with qBittorrent 4.x names.
        let h1 = "1111111111111111111111111111111111111111".to_string();
        let form = reqwest::multipart::Form::new()
            .text("urls", format!("magnet:?xt=urn:btih:{h1}&dn=one"))
            .text("savepath", format!("{base}/m1"))
            .text("category", "Cat")
            .text("paused", "true")
            .text("root_folder", "false");
        let r = h.client.post(h.url("/api/v2/torrents/add")).header("Cookie", &c).multipart(form).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let t = info(h1).await;
        assert_eq!(t["save_path"], serde_json::json!(format!("{base}/m1")));
        assert_eq!(t["category"], "Cat");
        assert!(t["state"].as_str().unwrap().starts_with("stopped"), "paused is an alias of stopped: {t}");

        // The same, urlencoded.
        let h2 = "2222222222222222222222222222222222222222".to_string();
        let urls = format!("magnet:?xt=urn:btih:{h2}&dn=two");
        let m2 = format!("{base}/m2");
        let form = [("urls", urls.as_str()), ("savepath", m2.as_str()), ("tags", "u"), ("stopped", "true")];
        let r = h.post("/api/v2/torrents/add", &form, Some(&c)).await;
        assert_eq!(r.status(), 200);
        let t = info(h2).await;
        assert_eq!(t["save_path"], serde_json::json!(m2));
        assert_eq!(t["tags"], "u");
        assert!(t["state"].as_str().unwrap().starts_with("stopped"), "{t}");
    });
}

#[test]
fn edit_category_keeps_what_the_call_leaves_out() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let create = [
            ("category", "films"),
            ("savePath", "/films"),
            ("downloadPathEnabled", "true"),
            ("downloadPath", "/incoming"),
            ("ratioLimit", "2.5"),
            ("seedingTimeLimit", "90"),
            ("shareLimitsMode", "MatchAll"),
            ("shareLimitAction", "Remove"),
        ];
        assert_eq!(h.post("/api/v2/torrents/createCategory", &create, Some(&c)).await.status(), 200);
        let films = |v: Value| v["films"].clone();

        // savePath is optional now; nothing else is reset.
        let r = h.post("/api/v2/torrents/editCategory", &[("category", "films"), ("inactiveSeedingTimeLimit", "30")], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let cat = films(h.get("/api/v2/torrents/categories", Some(&c)).await.json().await.unwrap());
        assert_eq!(cat["savePath"], "/films");
        assert_eq!(cat["download_path"], "/incoming");
        assert_eq!(cat["ratio_limit"], 2.5);
        assert_eq!(cat["seeding_time_limit"], 90);
        assert_eq!(cat["inactive_seeding_time_limit"], 30);
        assert_eq!(cat["share_limit_action"], "Remove");
        assert_eq!(cat["share_limits_mode"], "MatchAll");

        // An empty downloadPathEnabled goes back to the default.
        let r = h.post("/api/v2/torrents/editCategory", &[("category", "films"), ("downloadPathEnabled", "")], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let cat = films(h.get("/api/v2/torrents/categories", Some(&c)).await.json().await.unwrap());
        assert!(cat["download_path"].is_null(), "{cat}");
        assert_eq!(cat["ratio_limit"], 2.5);

        // Bad values are refused instead of resetting the option.
        for bad in [
            ("ratioLimit", "-3"),
            ("seedingTimeLimit", "x"),
            ("shareLimitAction", "Explode"),
            ("shareLimitsMode", "MatchSome"),
            ("shareLimitsMode", "matchall"),
            ("downloadPathEnabled", "maybe"),
        ] {
            let r = h.post("/api/v2/torrents/editCategory", &[("category", "films"), bad], Some(&c)).await;
            assert_eq!(r.status(), 400, "{bad:?}");
        }
        let cat = films(h.get("/api/v2/torrents/categories", Some(&c)).await.json().await.unwrap());
        assert_eq!(cat["share_limits_mode"], "MatchAll", "a refused value changes nothing");
        let r = h.post("/api/v2/torrents/createCategory", &[("category", "bad"), ("shareLimitsMode", "Any")], Some(&c)).await;
        assert_eq!(r.status(), 400);
        let r = h.post("/api/v2/torrents/editCategory", &[("category", "films"), ("shareLimitsMode", "Default")], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let cat = films(h.get("/api/v2/torrents/categories", Some(&c)).await.json().await.unwrap());
        assert_eq!(cat["share_limits_mode"], "Default");
        let r = h.post("/api/v2/torrents/editCategory", &[("category", "nope"), ("savePath", "/x")], Some(&c)).await;
        assert_eq!(r.status(), 404);
    });
}

/// qBittorrent 5.3's share limits mode on torrents: `torrents/add`,
/// `torrents/info` and `setShareLimits` (where the mode stays optional).
#[test]
fn torrent_share_limits_mode() {
    let h = Harness::new(|_| {});
    let base = h._dir.path().to_string_lossy().into_owned();
    h.block(async {
        let c = h.login().await;
        let info = |hash: String| {
            let (h, c) = (&h, c.clone());
            async move {
                let r = h.get(&format!("/api/v2/torrents/info?hashes={hash}"), Some(&c)).await;
                let v: serde_json::Value = r.json().await.unwrap();
                v.as_array().and_then(|a| a.first()).cloned().unwrap_or_default()
            }
        };
        let id = "3333333333333333333333333333333333333333".to_string();
        let urls = format!("magnet:?xt=urn:btih:{id}&dn=three");
        let save = format!("{base}/m3");
        let form = [("urls", urls.as_str()), ("savepath", save.as_str()), ("stopped", "true"), ("shareLimitsMode", "MatchAll")];
        assert_eq!(h.post("/api/v2/torrents/add", &form, Some(&c)).await.status(), 200);
        let t = info(id.clone()).await;
        assert_eq!(t["share_limits_mode"], "MatchAll");

        // Without shareLimitsMode (clients before 5.3) the mode goes back to Default.
        let limits = [
            ("hashes", id.as_str()),
            ("ratioLimit", "2"),
            ("seedingTimeLimit", "-2"),
            ("inactiveSeedingTimeLimit", "-2"),
            ("shareLimitAction", "Stop"),
        ];
        assert_eq!(h.post("/api/v2/torrents/setShareLimits", &limits, Some(&c)).await.status(), 200);
        let t = info(id.clone()).await;
        assert_eq!((t["share_limits_mode"].as_str(), t["ratio_limit"].as_f64()), (Some("Default"), Some(2.0)));
        let mut with_mode = limits.to_vec();
        with_mode.push(("shareLimitsMode", "MatchAny"));
        assert_eq!(h.post("/api/v2/torrents/setShareLimits", &with_mode, Some(&c)).await.status(), 200);
        assert_eq!(info(id.clone()).await["share_limits_mode"], "MatchAny");

        // The global mode.
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(prefs["share_limits_mode"], "MatchAny");
        let json = r#"{"share_limits_mode":"MatchAll"}"#;
        assert_eq!(h.post("/api/v2/app/setPreferences", &[("json", json)], Some(&c)).await.status(), 200);
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(prefs["share_limits_mode"], "MatchAll");
    });
}

/// qBittorrent 5.3's `transfer/pauseSession` / `resumeSession` (POST only),
/// `session_state` in `sync/maindata`'s `server_state`, and the
/// `start_paused` / `shutdown_timeout` preferences.
#[test]
fn pause_and_resume_session() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let maindata = |rid: i64| {
            let (h, c) = (&h, c.clone());
            async move {
                let v: Value = h.get(&format!("/api/v2/sync/maindata?rid={rid}"), Some(&c)).await.json().await.unwrap();
                v
            }
        };
        let first = maindata(0).await;
        assert_eq!(first["server_state"]["session_state"], false);
        let rid = first["rid"].as_i64().unwrap();

        assert_eq!(h.get("/api/v2/transfer/pauseSession", Some(&c)).await.status(), 405);
        assert!(!h.app.is_session_paused());
        assert_eq!(h.post("/api/v2/transfer/pauseSession", &[], Some(&c)).await.status(), 200);
        assert!(h.app.is_session_paused());
        // Pausing twice is harmless; the change arrives as a diff.
        assert_eq!(h.post("/api/v2/transfer/pauseSession", &[], Some(&c)).await.status(), 200);
        let diff = maindata(rid).await;
        assert_eq!(diff["server_state"]["session_state"], true);
        // Not part of transfer/info (as upstream).
        let info: Value = h.get("/api/v2/transfer/info", Some(&c)).await.json().await.unwrap();
        assert!(info.get("session_state").is_none());

        assert_eq!(h.post("/api/v2/transfer/resumeSession", &[], Some(&c)).await.status(), 200);
        assert!(!h.app.is_session_paused());
        assert_eq!(maindata(0).await["server_state"]["session_state"], false);
        assert_eq!(h.post("/api/v2/transfer/resumeSession", &[], None).await.status(), 403);

        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!((prefs["start_paused"].clone(), prefs["shutdown_timeout"].clone()), (json!(false), json!(-1)));
        let json = r#"{"start_paused":true,"shutdown_timeout":45}"#;
        assert_eq!(h.post("/api/v2/app/setPreferences", &[("json", json)], Some(&c)).await.status(), 200);
        assert!(h.app.preferences().advanced.start_paused);
        assert_eq!(h.app.preferences().advanced.shutdown_timeout, 45);
        // Only a later start is paused.
        assert!(!h.app.is_session_paused());
        let json = r#"{"shutdown_timeout":-5}"#;
        assert_eq!(h.post("/api/v2/app/setPreferences", &[("json", json)], Some(&c)).await.status(), 200);
        assert_eq!(h.app.preferences().advanced.shutdown_timeout, 45);
    });
}

/// Polls `pred` for up to 30 s.
async fn eventually(what: &str, mut pred: impl FnMut() -> bool) {
    let start = Instant::now();
    while !pred() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// qBittorrent 5.3's .torrent file backups: the settings, the backup of an
/// added torrent (named after it, inside the profile's data folder by
/// default), its move to the finished folder and its removal with the
/// torrent.
#[test]
fn torrent_file_backups() {
    let h = Harness::new(|_| {});
    let base = h._dir.path().to_path_buf();
    h.block(async {
        let c = h.login().await;
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(prefs["torrent_files_backup_enabled"], false);
        assert_eq!(prefs["torrent_files_backup_dir"], "backup");
        assert!(prefs.get("export_dir").is_none() && prefs.get("export_dir_fin").is_none());

        let done = base.join("done");
        let json = json!({
            "torrent_files_backup_enabled": true,
            "torrent_files_finished_backup_dir_enabled": true,
            "torrent_files_finished_backup_dir": done.to_string_lossy(),
            "remove_torrent_file_backup": true,
        });
        let r = h.post("/api/v2/app/setPreferences", &[("json", &json.to_string())], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!(prefs["torrent_files_finished_backup_dir"], json!(done.to_string_lossy()));
        assert_eq!(prefs["remove_torrent_file_backup"], true);

        // A stopped torrent: backed up at once into data/backup.
        let tor = crate::migrate::libtorrent::tests::sample_torrent(16, &[7u8; 40], b"nfo!");
        let id = crate::bittorrent::metainfo::parse_torrent(&tor, Default::default()).unwrap().meta.id();
        let save = base.join("dl");
        let add = |stopped: &'static str| {
            let form = reqwest::multipart::Form::new()
                .part("torrents", reqwest::multipart::Part::bytes(tor.clone()).file_name("x.torrent"))
                .text("savepath", save.to_string_lossy().into_owned())
                .text("stopped", stopped);
            h.client.post(h.url("/api/v2/torrents/add")).header("Cookie", &c).multipart(form).send()
        };
        assert_eq!(add("true").await.unwrap().status(), 200);
        let backup = base.join("data/backup/dir.torrent");
        eventually("the backup", || backup.is_file()).await;
        let parsed = crate::bittorrent::metainfo::parse_torrent(&std::fs::read(&backup).unwrap(), Default::default()).unwrap();
        assert_eq!(parsed.meta.id(), id);

        // Removing the torrent removes its backup.
        let del = [("hashes", id.as_str()), ("deleteFiles", "false")];
        assert_eq!(h.post("/api/v2/torrents/delete", &del, Some(&c)).await.status(), 200);
        eventually("the backup to go", || !backup.exists()).await;

        // A file already named like the torrent is kept; the backup gets a
        // new name, and moves to the finished folder once the download
        // (data already on disk) is checked complete.
        std::fs::write(&backup, b"user file").unwrap();
        std::fs::create_dir_all(save.join("dir")).unwrap();
        std::fs::write(save.join("dir/a.mkv"), [7u8; 40]).unwrap();
        std::fs::write(save.join("dir/b.nfo"), b"nfo!").unwrap();
        assert_eq!(add("false").await.unwrap().status(), 200);
        let finished = done.join("dir.torrent");
        eventually("the backup in the finished folder", || finished.is_file()).await;
        assert!(!base.join("data/backup/dir (1).torrent").exists(), "moved, not copied");
        assert_eq!(std::fs::read(&backup).unwrap(), b"user file");

        assert_eq!(h.post("/api/v2/torrents/delete", &del, Some(&c)).await.status(), 200);
        eventually("the finished backup to go", || !finished.exists()).await;
        assert!(backup.exists());
    });
}

/// qBittorrent 5.3's `rss/exportRules` / `rss/importRules`: the JSON rules
/// file round-trips; exactly one JSON file is accepted.
#[test]
fn rss_rules_export_and_import() {
    let h = Harness::new(|_| {});
    h.block(async {
        let c = h.login().await;
        let rule = r#"{"mustContain":"show","enabled":true}"#;
        let r = h.post("/api/v2/rss/setRule", &[("ruleName", "Show"), ("ruleDef", rule)], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let r = h.get("/api/v2/rss/exportRules", Some(&c)).await;
        assert_eq!(r.status(), 200);
        assert!(r.headers()["content-disposition"].to_str().unwrap().contains("rss-downloader-rules.json"));
        let exported = r.bytes().await.unwrap().to_vec();
        let v: Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(v["Show"]["mustContain"], "show");

        let r = h.post("/api/v2/rss/removeRule", &[("ruleName", "Show")], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let upload = |parts: Vec<&'static [u8]>| {
            let mut form = reqwest::multipart::Form::new();
            for (i, p) in parts.into_iter().enumerate() {
                form = form.part(format!("file{i}"), reqwest::multipart::Part::bytes(p).file_name(format!("rules{i}.json")));
            }
            h.client.post(h.url("/api/v2/rss/importRules")).header("Cookie", &c).multipart(form).send()
        };
        let leaked: &'static [u8] = Box::leak(exported.into_boxed_slice());
        assert_eq!(upload(vec![leaked]).await.unwrap().status(), 200);
        let rules: Value = h.get("/api/v2/rss/rules", Some(&c)).await.json().await.unwrap();
        assert_eq!(rules["Show"]["mustContain"], "show");

        assert_eq!(upload(vec![]).await.unwrap().status(), 400, "no file");
        assert_eq!(upload(vec![leaked, leaked]).await.unwrap().status(), 400, "two files");
        assert_eq!(upload(vec![b"[Show]\nmust_contain=x\n"]).await.unwrap().status(), 400, "legacy format");
        assert_eq!(upload(vec![b"{not json"]).await.unwrap().status(), 400, "broken JSON");
        // importRules is POST only.
        assert_eq!(h.get("/api/v2/rss/importRules", Some(&c)).await.status(), 405);
    });
}

/// qBittorrent 5.3's `torrents/downloadFile`: a complete file by index or
/// path, streamed as an attachment; incomplete files, bad indexes and
/// paths, and links leaving the torrent folder are refused.
#[test]
fn download_completed_files() {
    let h = Harness::new(|_| {});
    let base = h._dir.path().to_path_buf();
    h.block(async {
        let c = h.login().await;
        let tor = crate::migrate::libtorrent::tests::sample_torrent(16, &[7u8; 48], b"nfo!");
        let id = crate::bittorrent::metainfo::parse_torrent(&tor, Default::default()).unwrap().meta.id();
        let save = base.join("dl");
        std::fs::create_dir_all(save.join("dir")).unwrap();
        // 48 bytes: a.mkv fills three whole pieces, so it completes without b.nfo.
        std::fs::write(save.join("dir/a.mkv"), [7u8; 48]).unwrap();
        let form = reqwest::multipart::Form::new()
            .part("torrents", reqwest::multipart::Part::bytes(tor.clone()).file_name("x.torrent"))
            .text("savepath", save.to_string_lossy().into_owned())
            .text("stopped", "false");
        let r = h.client.post(h.url("/api/v2/torrents/add")).header("Cookie", &c).multipart(form).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let get = |file: &str| {
            let rb = h.client.get(h.url(&format!("/api/v2/torrents/downloadFile?hash={id}&file={file}"))).header("Cookie", c.clone());
            async move { rb.send().await.unwrap() }
        };
        // a.mkv is checked complete; b.nfo is missing.
        let mut last = String::new();
        for _ in 0..600 {
            let r = get("0").await;
            if r.status() == 200 {
                last.clear();
                break;
            }
            last = format!("{} {}", r.status(), r.text().await.unwrap());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(last.is_empty(), "a.mkv never became downloadable: {last}");
        let r = get("0").await;
        assert_eq!(r.headers()["content-type"], "application/octet-stream");
        assert_eq!(r.headers()["content-length"], "48");
        assert!(r.headers()["content-disposition"].to_str().unwrap().contains("a.mkv"));
        assert_eq!(r.bytes().await.unwrap().as_ref(), &[7u8; 48]);
        assert_eq!(get("dir%2Fa.mkv").await.status(), 200, "by relative path");
        let r = get("1").await;
        assert_eq!((r.status().as_u16(), r.text().await.unwrap()), (409, "File not fully downloaded".to_owned()));
        assert_eq!(get("2").await.text().await.unwrap(), "File index 2 is out of bounds");
        assert_eq!(get("-1").await.text().await.unwrap(), "\"-1\" is not a valid file index");
        assert_eq!(get("..%2F..%2Fetc%2Fpasswd").await.status(), 409);
        let r = h.get("/api/v2/torrents/downloadFile?hash=0123456789abcdef0123456789abcdef01234567&file=0", Some(&c)).await;
        assert_eq!(r.status(), 404);
        assert_eq!(h.get(&format!("/api/v2/torrents/downloadFile?hash={id}&file=0"), None).await.status(), 403, "login required");

        // A link swapped in for the completed file is not followed outside.
        let secret = base.join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        std::fs::remove_file(save.join("dir/a.mkv")).unwrap();
        std::os::unix::fs::symlink(&secret, save.join("dir/a.mkv")).unwrap();
        let r = get("0").await;
        assert_eq!(r.status(), 409);
        assert!(!r.text().await.unwrap().contains("secret"));
    });
}

#[test]
fn search_jobs_are_restored_and_shared_by_sessions() {
    let jobs = json!({"Jobs": [
        {"ID": 1234, "SearchPattern": "ubuntu", "Category": "software", "Plugins": ["piratebay", "eztv"]},
        {"ID": 42, "SearchPattern": "debian", "Category": "all", "Plugins": []},
    ]});
    let results = json!([
        {"FileName": "ubuntu.iso", "FileURL": "magnet:?xt=urn:btih:0000000000000000000000000000000000000001",
         "FileSize": 123, "SeedersCount": 4, "LeechersCount": 1, "EngineName": "piratebay",
         "SiteURL": "https://example.org", "DescrLink": "https://example.org/1", "PubDate": 1_700_000_000},
        {"FileName": "ubuntu2.iso", "FileURL": "u2", "FileSize": 5, "SeedersCount": 0, "LeechersCount": 0,
         "EngineName": "eztv", "SiteURL": "", "DescrLink": "", "PubDate": -1},
    ]);
    let h = Harness::build_with(
        |root, p| {
            p.behavior.store_search_jobs = true;
            p.behavior.store_search_job_results = true;
            let folder = root.join("data/WebUI/Search");
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("Session.json"), jobs.to_string()).unwrap();
            std::fs::write(folder.join("1234.json"), results.to_string()).unwrap();
        },
        |_| {},
        true,
    );
    let folder = h._dir.path().join("data/WebUI/Search");
    h.block(async {
        let c = h.login().await;
        let prefs: Value = h.get("/api/v2/app/preferences", Some(&c)).await.json().await.unwrap();
        assert_eq!((prefs["store_search_jobs"].clone(), prefs["store_search_job_results"].clone()), (json!(true), json!(true)));

        // Restored jobs are stopped and report what they were started with.
        let text = h.get("/api/v2/search/status", Some(&c)).await.text().await.unwrap();
        assert_eq!(
            text,
            concat!(
                r#"[{"id":1234,"status":"Stopped","pattern":"ubuntu","category":"software","plugins":["piratebay","eztv"],"total":2},"#,
                r#"{"id":42,"status":"Stopped","pattern":"debian","category":"all","plugins":[],"total":0}]"#
            )
        );
        let one: Value = h.get("/api/v2/search/status?id=42", Some(&c)).await.json().await.unwrap();
        assert_eq!(one.as_array().unwrap().len(), 1);
        assert_eq!(h.get("/api/v2/search/status?id=7", Some(&c)).await.status(), 404);

        let r: Value = h.get("/api/v2/search/results?id=1234&limit=1&offset=1", Some(&c)).await.json().await.unwrap();
        assert_eq!((r["status"].clone(), r["total"].clone()), (json!("Stopped"), json!(2)));
        assert_eq!(r["results"][0]["fileName"], "ubuntu2.iso");
        assert_eq!(r["results"][0]["pubDate"], -1);
        let r = h.get("/api/v2/search/results?id=1234&offset=3", Some(&c)).await;
        assert_eq!((r.status().as_u16(), r.text().await.unwrap()), (409, "Offset is out of range".to_owned()));

        // Jobs are shared: another session sees and deletes them.
        let other = h.login().await;
        let all: Value = h.get("/api/v2/search/status", Some(&other)).await.json().await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        assert_eq!(h.post("/api/v2/search/stop", &[("id", "1234")], Some(&other)).await.status(), 200);
        assert_eq!(h.post("/api/v2/search/delete", &[("id", "1234")], Some(&other)).await.status(), 200);
        assert!(!folder.join("1234.json").exists());
        assert_eq!(h.post("/api/v2/search/delete", &[("id", "1234")], Some(&c)).await.status(), 404);
        let session: Value = serde_json::from_slice(&std::fs::read(folder.join("Session.json")).unwrap()).unwrap();
        assert_eq!(session["Jobs"].as_array().unwrap().len(), 1);
        assert_eq!(session["Jobs"][0]["ID"], 42);

        // Turning the option off deletes the stored jobs.
        let r = h.post("/api/v2/app/setPreferences", &[("json", r#"{"store_search_jobs":false}"#)], Some(&c)).await;
        assert_eq!(r.status(), 200);
        let deadline = Instant::now() + Duration::from_secs(10);
        while folder.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!folder.exists());
    });
}
