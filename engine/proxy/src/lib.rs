//! Proxy connector for peer and tracker connections, covering qBittorrent's
//! proxy types: HTTP (CONNECT), SOCKS4 (SOCKS4a when resolving remotely) and
//! SOCKS5, with optional authentication and "Perform hostname lookup via
//! proxy".

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyKind {
    Http,
    Socks4,
    Socks5,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub kind: ProxyKind,
    /// Proxy host name or IP.
    pub host: String,
    pub port: u16,
    /// (username, password); SOCKS4 uses the username as user id.
    pub auth: Option<(String, String)>,
    /// Resolve target host names on the proxy (SOCKS4a/SOCKS5h/HTTP).
    pub remote_dns: bool,
}

// The password never appears in logs or error messages.
impl std::fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("auth", &self.auth.as_ref().map(|(user, _)| (user, "<redacted>")))
            .field("remote_dns", &self.remote_dns)
            .finish()
    }
}

/// Destination of a proxied connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Addr(SocketAddr),
    Host(String, u16),
}

impl Target {
    fn host_port(&self) -> String {
        match self {
            Target::Addr(SocketAddr::V6(a)) => format!("[{}]:{}", a.ip(), a.port()),
            Target::Addr(a) => a.to_string(),
            Target::Host(h, p) => format!("{h}:{p}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("proxy refused the connection: {0}")]
    Refused(String),
    #[error("SOCKS error: {0}")]
    Socks(#[from] tokio_socks::Error),
    #[error("invalid proxy response")]
    BadResponse,
    #[error("connection through proxy timed out")]
    Timeout,
    #[error("proxy type does not support this target")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, ProxyError>;

const MAX_HTTP_RESPONSE_HEAD: usize = 16 * 1024;

/// Opens a TCP stream to `target` through `proxy` within `timeout`.
pub async fn connect(proxy: &ProxyConfig, target: Target, timeout: Duration) -> Result<TcpStream> {
    tokio::time::timeout(timeout, connect_inner(proxy, target)).await.map_err(|_| ProxyError::Timeout)?
}

async fn connect_inner(proxy: &ProxyConfig, target: Target) -> Result<TcpStream> {
    let proxy_addr = (proxy.host.as_str(), proxy.port);
    let target = resolve_locally_if_needed(proxy, target).await?;
    match proxy.kind {
        ProxyKind::Http => http_connect(proxy, target).await,
        ProxyKind::Socks5 => {
            let s = match (&proxy.auth, &target) {
                (Some((u, p)), Target::Addr(a)) => {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(proxy_addr, *a, u, p).await?
                }
                (Some((u, p)), Target::Host(h, port)) => {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(proxy_addr, (h.as_str(), *port), u, p).await?
                }
                (None, Target::Addr(a)) => tokio_socks::tcp::Socks5Stream::connect(proxy_addr, *a).await?,
                (None, Target::Host(h, port)) => {
                    tokio_socks::tcp::Socks5Stream::connect(proxy_addr, (h.as_str(), *port)).await?
                }
            };
            Ok(s.into_inner())
        }
        ProxyKind::Socks4 => {
            if let Target::Addr(SocketAddr::V6(_)) = target {
                return Err(ProxyError::Unsupported);
            }
            let s = match (&proxy.auth, &target) {
                (Some((u, _)), Target::Addr(a)) => {
                    tokio_socks::tcp::Socks4Stream::connect_with_userid(proxy_addr, *a, u).await?
                }
                (Some((u, _)), Target::Host(h, port)) => {
                    tokio_socks::tcp::Socks4Stream::connect_with_userid(proxy_addr, (h.as_str(), *port), u).await?
                }
                (None, Target::Addr(a)) => tokio_socks::tcp::Socks4Stream::connect(proxy_addr, *a).await?,
                (None, Target::Host(h, port)) => {
                    tokio_socks::tcp::Socks4Stream::connect(proxy_addr, (h.as_str(), *port)).await?
                }
            };
            Ok(s.into_inner())
        }
    }
}

async fn resolve_locally_if_needed(proxy: &ProxyConfig, target: Target) -> Result<Target> {
    match target {
        Target::Host(h, p) if !proxy.remote_dns => {
            let addr = tokio::net::lookup_host((h.as_str(), p))
                .await?
                .next()
                .ok_or_else(|| ProxyError::Refused(format!("cannot resolve {h}")))?;
            Ok(Target::Addr(addr))
        }
        t => Ok(t),
    }
}

async fn http_connect(proxy: &ProxyConfig, target: Target) -> Result<TcpStream> {
    let mut s = TcpStream::connect((proxy.host.as_str(), proxy.port)).await?;
    let hp = target.host_port();
    let mut req = format!("CONNECT {hp} HTTP/1.1\r\nHost: {hp}\r\n");
    if let Some((u, p)) = &proxy.auth {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
        req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await?;

    // Read the response head byte by byte so no tunnelled data is consumed.
    let mut head = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HTTP_RESPONSE_HEAD {
            return Err(ProxyError::BadResponse);
        }
        if s.read(&mut byte).await? == 0 {
            return Err(ProxyError::BadResponse);
        }
        head.push(byte[0]);
    }
    let status_line = head.split(|&b| b == b'\n').next().unwrap_or_default();
    let status_line = String::from_utf8_lossy(status_line);
    let mut parts = status_line.split_whitespace();
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return Err(ProxyError::BadResponse);
    };
    if !version.starts_with("HTTP/1.") {
        return Err(ProxyError::BadResponse);
    }
    if !code.starts_with('2') {
        return Err(ProxyError::Refused(status_line.trim().to_string()));
    }
    Ok(s)
}

#[cfg(test)]
mod tests;
