use std::io::Write as _;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::net::testutil;

/// Requests seen by the test server: (path, raw head).
type Log = Arc<Mutex<Vec<(String, String)>>>;

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Minimal HTTP/1.1 server with canned routes; one request per connection.
async fn serve(log: Log) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let log = log.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().push((path.clone(), head.clone()));
                let cookie = head
                    .lines()
                    .find_map(|l| l.strip_prefix("cookie: ").or_else(|| l.strip_prefix("Cookie: ")))
                    .unwrap_or("")
                    .to_string();
                let simple = |status: &str, headers: &str, body: &[u8]| {
                    let mut r = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(body);
                    r
                };
                let resp: Vec<u8> = match path.as_str() {
                    "/plain" => simple("200 OK", "Content-Type: text/plain\r\nSet-Cookie: sid=abc; Path=/; Max-Age=3600\r\n", b"hello"),
                    "/echo-cookie" => simple("200 OK", "", cookie.as_bytes()),
                    "/echo-auth" => {
                        let auth = head
                            .lines()
                            .find_map(|l| l.strip_prefix("authorization: ").or_else(|| l.strip_prefix("Authorization: ")))
                            .unwrap_or("")
                            .to_string();
                        simple("200 OK", "", auth.as_bytes())
                    }
                    "/same-host-auth" => simple("302 Found", "Location: /echo-auth\r\n", b""),
                    "/cross-host-auth" => simple("302 Found", &format!("Location: http://localhost:{port}/echo-auth\r\n"), b""),
                    "/to-magnet" => simple("302 Found", "Location: magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567\r\n", b""),
                    "/to-file" => simple("302 Found", "Location: file:///etc/passwd\r\n", b""),
                    "/cross-host" => simple("302 Found", &format!("Location: http://localhost:{port}/echo-cookie\r\n"), b""),
                    "/big-cl" => simple("200 OK", "", &vec![b'x'; 300_000]),
                    "/gzip" => simple("200 OK", "Content-Encoding: gzip\r\n", &gzip(&vec![b'z'; 1_000_000])),
                    "/slow" => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        simple("200 OK", "", b"late")
                    }
                    "/big-stream" => {
                        // Chunked body without a length: 20 x 64 KiB.
                        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await;
                        let chunk = vec![b'y'; 65536];
                        for _ in 0..20 {
                            let mut c = format!("{:x}\r\n", chunk.len()).into_bytes();
                            c.extend_from_slice(&chunk);
                            c.extend_from_slice(b"\r\n");
                            if sock.write_all(&c).await.is_err() {
                                return;
                            }
                        }
                        let _ = sock.write_all(b"0\r\n\r\n").await;
                        return;
                    }
                    p if p.starts_with("/redirect/") => {
                        let n: u32 = p["/redirect/".len()..].parse().unwrap_or(0);
                        if n == 0 {
                            simple("200 OK", "", b"done")
                        } else {
                            simple("302 Found", &format!("Location: /redirect/{}\r\n", n - 1), b"")
                        }
                    }
                    _ => simple("404 Not Found", "", b"missing"),
                };
                let _ = sock.write_all(&resp).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    port
}

fn setup() -> (tokio::runtime::Runtime, tempfile::TempDir, CoreContext, Log, u16) {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let ctx = testutil::context(&rt, dir.path());
    let log: Log = Arc::default();
    let port = rt.block_on(serve(log.clone()));
    (rt, dir, ctx, log, port)
}

fn get(rt: &tokio::runtime::Runtime, ctx: &CoreContext, req: HttpRequest) -> QbtResult<HttpResponse> {
    rt.block_on(fetch(ctx, req, HttpPurpose::General))
}

#[test]
fn basic_get_cookies_and_headers() {
    let (rt, dir, ctx, log, port) = setup();
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/plain"))).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.body, b"hello");
    assert_eq!(r.content_type, "text/plain");
    let head = log.lock()[0].1.to_ascii_lowercase();
    assert!(head.contains("user-agent: mozilla/5.0"), "{head}");
    assert!(head.contains(&format!("referer: http://127.0.0.1:{port}/plain")));

    // The cookie set by /plain is sent back, and persisted with mode 0600.
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/echo-cookie"))).unwrap();
    assert_eq!(r.body, b"sid=abc");
    assert_eq!(cookies(&ctx).len(), 1);
    let file = dir.path().join("config/cookies.json");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !file.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);

    // Non-2xx is Ok with the status.
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/nope"))).unwrap();
    assert_eq!(r.status, 404);

    // Extra cookie header is merged.
    let req = HttpRequest { cookie: Some("extra=1".into()), ..HttpRequest::get(format!("http://127.0.0.1:{port}/echo-cookie")) };
    assert_eq!(get(&rt, &ctx, req).unwrap().body, b"sid=abc; extra=1");

    // set_cookies replaces the jar.
    set_cookies(&ctx, vec![]).unwrap();
    assert!(cookies(&ctx).is_empty());
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/echo-cookie"))).unwrap();
    assert_eq!(r.body, b"");
}

#[test]
fn redirects() {
    let (rt, _dir, ctx, _log, port) = setup();
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/redirect/{MAX_REDIRECTS}"))).unwrap();
    assert_eq!(r.body, b"done");
    assert_eq!(r.url, format!("http://127.0.0.1:{port}/redirect/0"));

    let e = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/redirect/{}", MAX_REDIRECTS + 1)))
        .unwrap_err();
    assert!(e.to_string().contains("Exceeded max redirections"), "{e}");

    let e = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/to-file"))).unwrap_err();
    assert!(e.to_string().contains("unsupported or dangerous protocol"), "{e}");

    let r = get(&rt, &ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/to-magnet"))).unwrap();
    assert!(r.is_magnet_redirect());
    assert!(r.url.starts_with("magnet:?xt=urn:btih:0123"));
    assert!(r.body.is_empty());

    // The caller's extra cookie never follows a redirect to another host.
    let req = HttpRequest { cookie: Some("secret=1".into()), ..HttpRequest::get(format!("http://127.0.0.1:{port}/cross-host")) };
    assert_eq!(get(&rt, &ctx, req).unwrap().body, b"");

    // Basic auth (DynDNS) follows same-host redirects only.
    let opts = FetchOptions { user_agent: None, basic_auth: Some(("user".into(), "pw".into())) };
    let auth = |path: &str| {
        rt.block_on(fetch_with(&ctx, HttpRequest::get(format!("http://127.0.0.1:{port}{path}")), HttpPurpose::General, opts.clone()))
            .unwrap()
            .body
    };
    assert_eq!(auth("/same-host-auth"), b"Basic dXNlcjpwdw==");
    assert_eq!(auth("/cross-host-auth"), b"");

    for bad in ["file:///etc/passwd", "ftp://example.com/x", "not a url", "magnet:?xt=urn:btih:00"] {
        assert!(get(&rt, &ctx, HttpRequest::get(bad)).is_err(), "{bad}");
    }
}

#[test]
fn size_limits_while_streaming() {
    let (rt, _dir, ctx, _log, port) = setup();
    let req = |path: &str, max: u64| HttpRequest { max_size: max, ..HttpRequest::get(format!("http://127.0.0.1:{port}{path}")) };

    let e = get(&rt, &ctx, req("/big-cl", 100_000)).unwrap_err();
    assert!(e.to_string().contains("exceeds the download limit"), "{e}");
    assert_eq!(get(&rt, &ctx, req("/big-cl", 300_000)).unwrap().body.len(), 300_000);

    // No Content-Length: aborted once the streamed body passes the limit.
    let e = get(&rt, &ctx, req("/big-stream", 150_000)).unwrap_err();
    assert!(e.to_string().contains("exceeds the download limit"), "{e}");

    // Gzip: the limit applies to the decompressed size (zip bombs).
    let e = get(&rt, &ctx, req("/gzip", 100_000)).unwrap_err();
    assert!(e.to_string().contains("exceeds the download limit"), "{e}");
    let r = get(&rt, &ctx, req("/gzip", 2_000_000)).unwrap();
    assert_eq!(r.body.len(), 1_000_000);
}

#[test]
fn total_timeout() {
    let (rt, _dir, ctx, _log, port) = setup();
    let req = HttpRequest { timeout_secs: 1, ..HttpRequest::get(format!("http://127.0.0.1:{port}/slow")) };
    let start = std::time::Instant::now();
    let e = get(&rt, &ctx, req).unwrap_err();
    assert!(e.to_string().contains("timed out"), "{e}");
    assert!(start.elapsed() < Duration::from_secs(4));
}

#[test]
fn download_to_file() {
    let (rt, dir, ctx, _log, port) = setup();
    let dest = dir.path().join("dl/file.bin");
    let r = rt
        .block_on(fetch_to_file(&ctx, HttpRequest::get(format!("http://127.0.0.1:{port}/big-cl")), HttpPurpose::General, &dest))
        .unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 300_000);
    // Failure: no file and no leftovers.
    let dest2 = dir.path().join("dl/other.bin");
    let req = HttpRequest { max_size: 10, ..HttpRequest::get(format!("http://127.0.0.1:{port}/big-cl")) };
    assert!(rt.block_on(fetch_to_file(&ctx, req, HttpPurpose::General, &dest2)).is_err());
    assert!(!dest2.exists());
    assert_eq!(std::fs::read_dir(dir.path().join("dl")).unwrap().count(), 1);
}

#[test]
fn proxy_urls() {
    let mut p = Preferences::default();
    assert_eq!(proxy_url(&p, HttpPurpose::General), None);
    p.connection.proxy_type = ProxyType::Socks5;
    p.connection.proxy_ip = "10.0.0.1".into();
    p.connection.proxy_port = 1080;
    // Profile flags select the purposes.
    assert_eq!(proxy_url(&p, HttpPurpose::General), None);
    p.connection.proxy_misc = true;
    assert_eq!(proxy_url(&p, HttpPurpose::General).as_deref(), Some("socks5h://10.0.0.1:1080"));
    assert_eq!(proxy_url(&p, HttpPurpose::Rss), None);
    p.connection.proxy_hostname_lookup = false;
    assert_eq!(proxy_url(&p, HttpPurpose::General).as_deref(), Some("socks5://10.0.0.1:1080"));
    p.connection.proxy_auth_enabled = true;
    p.connection.proxy_username = "us er".into();
    p.connection.proxy_password = "p@ss:w/rd".into();
    assert_eq!(proxy_url(&p, HttpPurpose::General).as_deref(), Some("socks5://us%20er:p%40ss%3Aw%2Frd@10.0.0.1:1080"));
    p.connection.proxy_type = ProxyType::Http;
    p.connection.proxy_ip = "::1".into();
    p.connection.proxy_rss = true;
    assert_eq!(proxy_url(&p, HttpPurpose::Rss).as_deref(), Some("http://us%20er:p%40ss%3Aw%2Frd@[::1]:1080"));
    p.connection.proxy_type = ProxyType::Socks4;
    p.connection.proxy_hostname_lookup = true;
    assert_eq!(proxy_url(&p, HttpPurpose::Rss).as_deref(), Some("socks4a://[::1]:1080"));
    // An enabled but broken proxy fails closed.
    p.connection.proxy_ip = "bad host/x".into();
    assert!(proxy_url_checked(&p.connection, HttpPurpose::Rss).is_err());
    p.connection.proxy_ip = String::new();
    assert!(proxy_url_checked(&p.connection, HttpPurpose::Rss).is_err());
}

#[test]
fn broken_proxy_fails_closed() {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let mut prefs = Preferences::default();
    prefs.connection.proxy_type = ProxyType::Http;
    prefs.connection.proxy_misc = true;
    prefs.connection.proxy_ip = String::new();
    let ctx = testutil::context_with(&rt, dir.path(), prefs);
    let e = rt.block_on(fetch(&ctx, HttpRequest::get("http://127.0.0.1:9/"), HttpPurpose::General)).unwrap_err();
    assert!(e.to_string().contains("proxy"), "{e}");
}

#[test]
fn messages_never_leak_credentials() {
    assert_eq!(
        scrub_userinfo("proxy error at socks5://user:secret@1.2.3.4:1080 refused"),
        "proxy error at socks5://***@1.2.3.4:1080 refused"
    );
    assert_eq!(scrub_userinfo("plain http://host/path"), "plain http://host/path");
    let u = Url::parse("https://user:pw@example.com/a?b=1").unwrap();
    assert_eq!(redact(&u), "https://example.com/a?b=1");
}

#[test]
fn response_url_never_carries_credentials() {
    // Callers (RSS) log the final URL of a download.
    let (rt, _dir, ctx, _log, port) = setup();
    let r = get(&rt, &ctx, HttpRequest::get(format!("http://feeduser:s3cret@127.0.0.1:{port}/plain"))).unwrap();
    assert_eq!(r.status, 200);
    assert!(!r.url.contains("s3cret") && !r.url.contains("feeduser"), "{}", r.url);
}

#[test]
fn user_agent_versions() {
    assert!(browser_user_agent_at(1_713_225_600).contains("rv:125.0) Gecko/20100101 Firefox/125.0"));
    // 2026-10-14: 29 whole (average) months later.
    assert!(browser_user_agent_at(1_791_936_000).contains("Firefox/154.0"));
}
