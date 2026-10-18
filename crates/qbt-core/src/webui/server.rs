//! HTTP/1.1 (+ optional TLS) server for the WebUI: listener binding,
//! connection limit, TLS handshake, timeouts and graceful shutdown. Requests
//! go through an axum `Router` whose fallback is the WebUI pipeline.
//!
//! Limits (qBittorrent's `Http::Server` plus hardening):
//! * at most 500 concurrent connections (extra ones are closed at once);
//! * 7 s to send a request head, which is also the keep-alive idle timeout
//!   (slow-loris protection);
//! * 64 KiB request head, 100 headers;
//! * 10 s TLS handshake timeout; TLS 1.2/1.3 with rustls' AEAD-only suites.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Request, State};
use axum::response::Response;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinHandle;

use super::webapp::{ConnInfo, WebApp};

pub const CONNECTIONS_LIMIT: usize = 500;
const HEAD_TIMEOUT: Duration = Duration::from_secs(7);
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD_SIZE: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;
const GRACEFUL_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum size of the certificate / key files.
pub const MAX_SSL_FILE_SIZE: u64 = 1024 * 1024;

fn listen(addr: SocketAddr, v6_only: Option<bool>) -> io::Result<std::net::TcpListener> {
    let s = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    if let Some(v) = v6_only {
        s.set_only_v6(v)?;
    }
    s.set_reuse_address(true)?;
    s.set_nonblocking(true)?;
    s.bind(&addr.into())?;
    s.listen(1024)?;
    Ok(s.into())
}

/// Binds the WebUI address: `*`/empty = every IPv4 and IPv6 address,
/// `0.0.0.0` = IPv4 only, `::` = IPv6 only, otherwise that address.
pub fn bind(address: &str, port: u16) -> Result<std::net::TcpListener, String> {
    let a = address.trim();
    let r = match a {
        "" | "*" => listen(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port), Some(false))
            .or_else(|_| listen(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port), None)),
        _ => {
            let ip = super::subnet::parse_ip(a).ok_or_else(|| "invalid IP address".to_owned())?;
            let v6_only = ip.is_ipv6().then_some(true);
            listen(SocketAddr::new(ip, port), v6_only)
        }
    };
    r.map_err(|e| e.to_string())
}

/// Builds the TLS configuration from PEM data (certificate chain + key).
pub fn tls_config(cert_pem: &[u8], key_pem: &[u8]) -> Result<Arc<rustls::ServerConfig>, String> {
    use rustls_pki_types::pem::PemObject as _;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("invalid certificate: {e}"))?;
    if certs.is_empty() {
        return Err("no certificate found".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| format!("invalid private key: {e}"))?;
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate and key do not match: {e}"))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(cfg))
}

/// A listening server.
pub struct RunningServer {
    shutdown: watch::Sender<bool>,
    connections: Arc<Semaphore>,
    accept_task: JoinHandle<()>,
    pub local_addr: SocketAddr,
}

impl RunningServer {
    /// Starts accepting connections (must run inside the runtime).
    pub fn start(
        listener: std::net::TcpListener,
        web: Arc<WebApp>,
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let local_addr = listener.local_addr()?;
        let (shutdown, rx) = watch::channel(false);
        let connections = Arc::new(Semaphore::new(CONNECTIONS_LIMIT));
        let conns = connections.clone();
        let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
        let accept_task = tokio::spawn(async move {
            let mut stop = rx.clone();
            loop {
                tokio::select! {
                    r = listener.accept() => match r {
                        Ok((stream, peer)) => {
                            let Ok(permit) = conns.clone().try_acquire_owned() else {
                                // Too many connections: close it right away.
                                drop(stream);
                                continue;
                            };
                            let local = stream.local_addr().unwrap_or(local_addr);
                            let info = ConnInfo {
                                peer: peer.ip().to_canonical(),
                                local: SocketAddr::new(local.ip().to_canonical(), local.port()),
                                tls: acceptor.is_some(),
                            };
                            tokio::spawn(serve(stream, info, acceptor.clone(), web.clone(), rx.clone(), permit));
                        }
                        // Out of descriptors etc.: back off instead of spinning.
                        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                    },
                    _ = stop.changed() => break,
                }
            }
        });
        Ok(Self { shutdown, connections, accept_task, local_addr })
    }

    /// Stops accepting, closes connections gracefully (bounded wait).
    pub async fn stop(self) {
        self.shutdown.send_replace(true);
        let _ = self.accept_task.await;
        let all = CONNECTIONS_LIMIT as u32;
        let _ = tokio::time::timeout(GRACEFUL_TIMEOUT + Duration::from_secs(1), self.connections.acquire_many(all)).await;
    }
}

async fn serve(
    stream: TcpStream,
    info: ConnInfo,
    tls: Option<tokio_rustls::TlsAcceptor>,
    web: Arc<WebApp>,
    shutdown: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let _ = stream.set_nodelay(true);
    match tls {
        None => serve_io(TokioIo::new(stream), info, web, shutdown).await,
        Some(acceptor) => {
            if let Ok(Ok(s)) = tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                serve_io(TokioIo::new(s), info, web, shutdown).await;
            }
        }
    }
}

/// Resolves once shutdown was requested (or the server went away).
async fn stopped(rx: &mut watch::Receiver<bool>) {
    loop {
        let stop = *rx.borrow_and_update();
        if stop || rx.changed().await.is_err() {
            return;
        }
    }
}

async fn handler(State(web): State<Arc<WebApp>>, Extension(info): Extension<ConnInfo>, req: Request) -> Response {
    web.handle(req, info).await
}

async fn serve_io<I>(io: I, info: ConnInfo, web: Arc<WebApp>, mut shutdown: watch::Receiver<bool>)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let router = axum::Router::new().fallback(handler).layer(Extension(info)).with_state(web);
    let svc = TowerToHyperService::new(router);
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HEAD_TIMEOUT)
        .keep_alive(true)
        .max_buf_size(MAX_HEAD_SIZE)
        .max_headers(MAX_HEADERS);
    let conn = builder.serve_connection(io, svc);
    tokio::pin!(conn);
    tokio::select! {
        _ = conn.as_mut() => {}
        _ = stopped(&mut shutdown) => {
            conn.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(GRACEFUL_TIMEOUT, conn).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_config_fails_closed_on_garbage() {
        assert!(tls_config(b"", b"").is_err());
        assert!(tls_config(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n", b"nope").is_err());
    }

    #[test]
    fn bind_addresses() {
        assert!(bind("not an ip", 0).is_err());
        let l = bind("127.0.0.1", 0).unwrap();
        assert!(l.local_addr().unwrap().ip().is_loopback());
        let any = bind("*", 0).unwrap();
        assert!(any.local_addr().unwrap().ip().is_unspecified());
    }
}
