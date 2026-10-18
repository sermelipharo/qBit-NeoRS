//! Peer TCP sockets: outgoing connections from a local port range, IP type of
//! service and socket buffer sizes.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use librqbit_utp::BindDevice;
use rand::RngExt;
use socket2::SockRef;
use tokio::net::{TcpSocket, TcpStream};
use tracing::debug;

/// Socket options applied to every peer TCP socket.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SocketTuning {
    pub tos: u8,
    pub send_buffer_size: u32,
    pub recv_buffer_size: u32,
}

impl SocketTuning {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Best effort: failures are logged and ignored (some options are not
    /// supported everywhere).
    pub fn apply(&self, sock: &SockRef<'_>, is_v6: bool) {
        if self.tos != 0 {
            let r = if is_v6 {
                sock.set_tclass_v6(u32::from(self.tos))
                    // Dual-stack sockets carrying IPv4 use IP_TOS.
                    .and_then(|_| sock.set_tos_v4(u32::from(self.tos)).or(Ok(())))
            } else {
                sock.set_tos_v4(u32::from(self.tos))
            };
            if let Err(e) = r {
                debug!("error setting peer socket type of service: {e:#}");
            }
        }
        if self.send_buffer_size > 0
            && let Err(e) = sock.set_send_buffer_size(self.send_buffer_size as usize)
        {
            debug!("error setting SO_SNDBUF: {e:#}");
        }
        if self.recv_buffer_size > 0
            && let Err(e) = sock.set_recv_buffer_size(self.recv_buffer_size as usize)
        {
            debug!("error setting SO_RCVBUF: {e:#}");
        }
    }

    pub fn apply_to_stream(&self, stream: &TcpStream) {
        if self.is_default() {
            return;
        }
        let is_v6 = stream.local_addr().map(|a| a.is_ipv6()).unwrap_or(false);
        self.apply(&SockRef::from(stream), is_v6);
    }
}

/// How many ports of the outgoing range are tried for one connection.
const MAX_PORT_ATTEMPTS: u32 = 32;

fn new_socket(
    addr: SocketAddr,
    bind_device: Option<&BindDevice>,
    tuning: &SocketTuning,
) -> std::io::Result<TcpSocket> {
    let sock = if addr.is_ipv6() {
        TcpSocket::new_v6()?
    } else {
        TcpSocket::new_v4()?
    };
    let sref = SockRef::from(&sock);
    if let Some(bd) = bind_device {
        bd.bind_sref(&sref, addr.is_ipv6())
            .map_err(std::io::Error::other)?;
    }
    tuning.apply(&sref, addr.is_ipv6());
    Ok(sock)
}

fn unspecified(addr: SocketAddr, port: u16) -> SocketAddr {
    if addr.is_ipv6() {
        (Ipv6Addr::UNSPECIFIED, port).into()
    } else {
        (Ipv4Addr::UNSPECIFIED, port).into()
    }
}

/// Connects to `addr`, binding the local port to one in `ports` (inclusive)
/// when given.
pub(crate) async fn tcp_connect(
    addr: SocketAddr,
    bind_device: Option<&BindDevice>,
    ports: Option<(u16, u16)>,
    tuning: &SocketTuning,
) -> std::io::Result<TcpStream> {
    let Some((lo, hi)) = ports else {
        return new_socket(addr, bind_device, tuning)?.connect(addr).await;
    };
    let span = u32::from(hi - lo) + 1;
    let start = rand::rng().random_range(0..span);
    let mut last_err = None;
    for i in 0..span.min(MAX_PORT_ATTEMPTS) {
        let port = lo + u16::try_from((start + i) % span).unwrap_or(0);
        let sock = new_socket(addr, bind_device, tuning)?;
        // Allows reusing ports in TIME_WAIT (libtorrent does the same).
        sock.set_reuseaddr(true)?;
        match sock.bind(unspecified(addr, port)) {
            Ok(()) => {}
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        }
        match sock.connect(addr).await {
            Ok(s) => return Ok(s),
            // The 4-tuple is in use (same port to the same peer): next port.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::AddrInUse
                ) =>
            {
                last_err = Some(e)
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no free outgoing port")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_from_port_range_with_tuning() {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        // Find a small free range.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = probe.local_addr().unwrap().port();
        drop(probe);
        let range = (base, base.saturating_add(3));
        let tuning = SocketTuning {
            tos: 0x20,
            send_buffer_size: 64 * 1024,
            recv_buffer_size: 64 * 1024,
        };
        let accept = tokio::spawn(async move {
            let mut out = Vec::new();
            for _ in 0..2 {
                out.push(l.accept().await.unwrap().1);
            }
            out
        });
        let s1 = tcp_connect(addr, None, Some(range), &tuning).await.unwrap();
        let s2 = tcp_connect(addr, None, Some(range), &tuning).await.unwrap();
        for s in [&s1, &s2] {
            let p = s.local_addr().unwrap().port();
            assert!((range.0..=range.1).contains(&p), "{p} not in {range:?}");
            assert_eq!(SockRef::from(s).tos_v4().unwrap(), 0x20);
        }
        assert_ne!(s1.local_addr().unwrap(), s2.local_addr().unwrap());
        let peers = accept.await.unwrap();
        assert_eq!(peers.len(), 2);

        // Without a range the OS picks.
        tcp_connect(addr, None, None, &SocketTuning::default())
            .await
            .unwrap_err();
    }
}
