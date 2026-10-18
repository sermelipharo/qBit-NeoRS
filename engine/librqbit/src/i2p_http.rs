//! `.i2p` HTTP trackers: a loopback HTTP proxy that tunnels requests for
//! `*.i2p` hosts through SAM. The tracker client gets a `reqwest` client
//! using this proxy (`TrackerClient::set_i2p_transport`).
//!
//! The proxy listens on 127.0.0.1 only and requires a random per-session
//! password (`Proxy-Authorization`), so other local programs can't use our
//! I2P session. It only connects to `*.i2p` hosts, never to the clearnet.

use std::{sync::Arc, time::Duration};

use base64::Engine;
use rand::RngExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, debug_span};
use tracker_comms::I2pTransport;

use crate::{Session, i2p::I2pRuntime};

const MAX_HEAD: usize = 16 * 1024;
const PROXY_USER: &str = "qbt";

/// The request line and headers of a proxied request.
#[derive(Debug, PartialEq, Eq)]
struct ProxyRequest {
    method: String,
    /// `*.i2p` host.
    host: String,
    /// Origin-form target (`/path?query`) for plain requests; `None` for CONNECT.
    path: Option<String>,
    /// Headers to forward (proxy headers removed, `Connection: close`).
    headers: Vec<(String, String)>,
    authorization: Option<String>,
}

fn parse_request(head: &str) -> Option<ProxyRequest> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let target = first.next()?;
    let version = first.next()?;
    if !version.starts_with("HTTP/1.") || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let (host, path) = if method == "CONNECT" {
        let host = target.rsplit_once(':').map_or(target, |(h, _)| h);
        (host.to_owned(), None)
    } else {
        let url = url::Url::parse(target).ok()?;
        if url.scheme() != "http" {
            return None;
        }
        let mut path = url.path().to_owned();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        (url.host_str()?.to_owned(), Some(path))
    };
    let host = host.to_ascii_lowercase();
    if !host.ends_with(".i2p")
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    {
        return None;
    }
    let mut headers = Vec::new();
    let mut authorization = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.split_once(':')?;
        let (k, v) = (k.trim(), v.trim());
        match k.to_ascii_lowercase().as_str() {
            "proxy-authorization" => authorization = Some(v.to_owned()),
            "proxy-connection" | "connection" | "keep-alive" => {}
            _ => headers.push((k.to_owned(), v.to_owned())),
        }
    }
    headers.push(("Connection".to_owned(), "close".to_owned()));
    Some(ProxyRequest {
        method,
        host,
        path,
        headers,
        authorization,
    })
}

async fn read_head(s: &mut TcpStream) -> anyhow::Result<String> {
    let mut head = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < MAX_HEAD, "request head too long");
        anyhow::ensure!(s.read(&mut b).await? == 1, "connection closed");
        head.push(b[0]);
    }
    Ok(String::from_utf8(head)?.trim_end().to_owned())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Local connections served by the `.i2p` tracker proxy at once.
const MAX_PROXY_CONNECTIONS: usize = 32;

async fn handle(
    mut client: TcpStream,
    runtime: Arc<I2pRuntime>,
    expected_auth: Arc<str>,
) -> anyhow::Result<()> {
    let head = tokio::time::timeout(Duration::from_secs(30), read_head(&mut client)).await??;
    let Some(req) = parse_request(&head) else {
        client
            .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    };
    let authorized = req
        .authorization
        .as_deref()
        .is_some_and(|a| constant_time_eq(a.as_bytes(), expected_auth.as_bytes()));
    if !authorized {
        client
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"i2p\"\r\nConnection: close\r\n\r\n")
            .await?;
        return Ok(());
    }
    let mut upstream = match runtime.connect(&req.host).await {
        Ok(s) => s,
        Err(e) => {
            debug!("I2P tracker connection failed: {e:#}");
            client
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n")
                .await?;
            return Ok(());
        }
    };
    match &req.path {
        None => {
            client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await?;
        }
        Some(path) => {
            let mut out = format!("{} {} HTTP/1.1\r\n", req.method, path);
            for (k, v) in &req.headers {
                out.push_str(k);
                out.push_str(": ");
                out.push_str(v);
                out.push_str("\r\n");
            }
            out.push_str("\r\n");
            upstream.write_all(out.as_bytes()).await?;
        }
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

impl Session {
    /// Starts the tunnelling proxy and points the tracker client's I2P
    /// transport at it.
    pub(crate) async fn start_i2p_http_proxy(
        self: &Arc<Self>,
        runtime: Arc<I2pRuntime>,
    ) -> anyhow::Result<()> {
        self.stop_i2p_http_proxy();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let password: String = {
            let mut rng = rand::rng();
            (0..24)
                .map(|_| char::from(b'a' + rng.random_range(0..26u8)))
                .collect()
        };
        let expected: Arc<str> = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{PROXY_USER}:{password}"))
        )
        .into();
        let token = self.cancellation_token().child_token();
        self.i2p.set_proxy_token(Some(token.clone()));

        let proxy =
            reqwest::Proxy::all(format!("http://{PROXY_USER}:{password}@127.0.0.1:{port}"))?;
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let weak = Arc::downgrade(self);
        self.tracker_ctx
            .client
            .set_i2p_transport(Some(I2pTransport {
                client,
                destination: runtime.session.destination.clone(),
                map_peer: Arc::new(move |p| weak.upgrade()?.connector.i2p.addr_for(p)),
            }));

        librqbit_core::spawn_utils::spawn_with_cancel(
            debug_span!(parent: self.rs(), "i2p_http_proxy", port),
            "i2p_http_proxy",
            token,
            async move {
                let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PROXY_CONNECTIONS));
                loop {
                    let (s, _) = listener.accept().await?;
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        debug!("I2P tracker proxy: too many connections");
                        continue;
                    };
                    let runtime = runtime.clone();
                    let expected = expected.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(e) = handle(s, runtime, expected).await {
                            debug!("I2P tracker proxy: {e:#}");
                        }
                    });
                }
                #[allow(unreachable_code)]
                Ok::<_, anyhow::Error>(())
            },
        );
        Ok(())
    }

    pub(crate) fn stop_i2p_http_proxy(&self) {
        self.tracker_ctx.client.set_i2p_transport(None);
        self.i2p.set_proxy_token(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_comparison() {
        assert!(constant_time_eq(b"Basic abc", b"Basic abc"));
        assert!(!constant_time_eq(b"Basic abd", b"Basic abc"));
        assert!(!constant_time_eq(b"Basic ab", b"Basic abc"));
    }

    #[test]
    fn parses_proxy_requests() {
        let r = parse_request(
            "GET http://Tracker.I2P/announce?info_hash=%01&x=1 HTTP/1.1\r\nHost: tracker.i2p\r\nProxy-Authorization: Basic abc\r\nProxy-Connection: keep-alive\r\nUser-Agent: x",
        )
        .unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.host, "tracker.i2p");
        assert_eq!(r.path.as_deref(), Some("/announce?info_hash=%01&x=1"));
        assert_eq!(r.authorization.as_deref(), Some("Basic abc"));
        assert_eq!(
            r.headers,
            vec![
                ("Host".to_owned(), "tracker.i2p".to_owned()),
                ("User-Agent".to_owned(), "x".to_owned()),
                ("Connection".to_owned(), "close".to_owned()),
            ]
        );

        let r = parse_request("CONNECT abc.b32.i2p:443 HTTP/1.1\r\nHost: x").unwrap();
        assert_eq!(r.host, "abc.b32.i2p");
        assert_eq!(r.path, None);

        // Clearnet hosts, other schemes and garbage are refused.
        assert!(parse_request("GET http://example.com/ HTTP/1.1").is_none());
        assert!(parse_request("CONNECT example.com:443 HTTP/1.1").is_none());
        assert!(parse_request("GET ftp://x.i2p/ HTTP/1.1").is_none());
        assert!(parse_request("GET /relative HTTP/1.1").is_none());
        assert!(parse_request("get http://x.i2p/ HTTP/1.1").is_none());
        assert!(parse_request("GET http://x.i2p/ SPDY").is_none());
        assert!(parse_request("GET http://x.i2p/ HTTP/1.1\r\nbroken header").is_none());
    }
}
