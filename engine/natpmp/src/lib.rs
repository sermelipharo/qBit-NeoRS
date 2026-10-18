//! NAT-PMP client (RFC 6886): external address discovery, TCP/UDP port
//! mappings and a background task that renews leases at half their lifetime
//! and removes them on shutdown.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::watch;

pub const NATPMP_PORT: u16 = 5351;
/// RFC 6886 recommends one hour.
pub const DEFAULT_LIFETIME: u32 = 3600;
const INITIAL_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_TRIES: u32 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Udp,
    Tcp,
}

impl Protocol {
    fn opcode(self) -> u8 {
        match self {
            Protocol::Udp => 1,
            Protocol::Tcp => 2,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NatPmpError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("no response from gateway")]
    Timeout,
    #[error("gateway returned result code {0}")]
    ResultCode(u16),
    #[error("malformed response")]
    Malformed,
    #[error("default gateway not found")]
    NoGateway,
}

pub type Result<T> = std::result::Result<T, NatPmpError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub protocol: Protocol,
    pub internal_port: u16,
    pub external_port: u16,
    pub lifetime: u32,
}

pub struct Client {
    socket: UdpSocket,
    gateway: SocketAddr,
}

impl Client {
    pub async fn new(gateway: Ipv4Addr) -> Result<Self> {
        Self::with_gateway_addr(SocketAddr::V4(SocketAddrV4::new(gateway, NATPMP_PORT))).await
    }

    /// Custom gateway address (tests use a local fake server).
    pub async fn with_gateway_addr(gateway: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.connect(gateway).await?;
        Ok(Self { socket, gateway })
    }

    pub fn gateway(&self) -> SocketAddr {
        self.gateway
    }

    async fn transact(&self, req: &[u8], expect_op: u8, expect_len: usize) -> Result<Vec<u8>> {
        let mut timeout = INITIAL_TIMEOUT;
        let mut buf = [0u8; 64];
        for _ in 0..MAX_TRIES {
            self.socket.send(req).await?;
            let deadline = tokio::time::Instant::now() + timeout;
            loop {
                match tokio::time::timeout_at(deadline, self.socket.recv(&mut buf)).await {
                    Err(_) => break,
                    Ok(Err(e)) => return Err(e.into()),
                    Ok(Ok(n)) => {
                        // Ignore stray packets (e.g. epoch announcements).
                        if n < 4 || buf[0] != 0 || buf[1] != expect_op {
                            continue;
                        }
                        let code = u16::from_be_bytes([buf[2], buf[3]]);
                        if code != 0 {
                            return Err(NatPmpError::ResultCode(code));
                        }
                        if n < expect_len {
                            return Err(NatPmpError::Malformed);
                        }
                        return Ok(buf[..n].to_vec());
                    }
                }
            }
            timeout *= 2;
        }
        Err(NatPmpError::Timeout)
    }

    pub async fn external_address(&self) -> Result<Ipv4Addr> {
        let r = self.transact(&[0, 0], 128, 12).await?;
        Ok(Ipv4Addr::new(r[8], r[9], r[10], r[11]))
    }

    /// Requests a mapping; `lifetime` 0 deletes it.
    pub async fn map(&self, protocol: Protocol, internal_port: u16, external_port: u16, lifetime: u32) -> Result<Mapping> {
        let mut req = [0u8; 12];
        req[1] = protocol.opcode();
        req[4..6].copy_from_slice(&internal_port.to_be_bytes());
        req[6..8].copy_from_slice(&external_port.to_be_bytes());
        req[8..12].copy_from_slice(&lifetime.to_be_bytes());
        let r = self.transact(&req, 128 + protocol.opcode(), 16).await?;
        Ok(Mapping {
            protocol,
            internal_port: u16::from_be_bytes([r[8], r[9]]),
            external_port: u16::from_be_bytes([r[10], r[11]]),
            lifetime: u32::from_be_bytes([r[12], r[13], r[14], r[15]]),
        })
    }
}

/// IPv4 default gateway of this host (macOS/BSD: `route -n get default`).
pub async fn default_gateway() -> Result<Ipv4Addr> {
    let out = tokio::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .await
        .map_err(|_| NatPmpError::NoGateway)?;
    parse_route_output(&String::from_utf8_lossy(&out.stdout)).ok_or(NatPmpError::NoGateway)
}

fn parse_route_output(s: &str) -> Option<Ipv4Addr> {
    s.lines()
        .find_map(|l| l.trim().strip_prefix("gateway:"))
        .and_then(|g| g.trim().parse().ok())
}

/// Keeps `ports` mapped until `stop` becomes true, then deletes the mappings.
/// Reports the external address and mappings through `status`.
pub async fn run_mapper(
    client: Client,
    ports: Vec<(Protocol, u16)>,
    lifetime: u32,
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<Option<(Ipv4Addr, Vec<Mapping>)>>,
) {
    let lifetime = if lifetime == 0 { DEFAULT_LIFETIME } else { lifetime };
    loop {
        let ext = client.external_address().await.ok();
        let mut maps = Vec::new();
        for &(proto, port) in &ports {
            match client.map(proto, port, port, lifetime).await {
                Ok(m) => maps.push(m),
                Err(e) => tracing::debug!(?proto, port, "NAT-PMP mapping failed: {e}"),
            }
        }
        let renew = maps.iter().map(|m| m.lifetime).min().unwrap_or(120).max(120) / 2;
        let _ = status.send(ext.map(|e| (e, maps)));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(renew as u64)) => {}
            _ = stop.wait_for(|s| *s) => break,
        }
    }
    for &(proto, port) in &ports {
        let _ = client.map(proto, port, 0, 0).await;
    }
}

#[cfg(test)]
mod tests;
