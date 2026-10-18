use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use arc_swap::{ArcSwap, ArcSwapOption};
use librqbit_core::hash_id::Id20;
use librqbit_mse::EncryptionPolicy;
use librqbit_utp::{BindDevice, UtpSocketUdp};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tracing::{debug, trace};

use crate::{
    Error, PeerConnectionOptions, Result,
    net::{
        NetSettings,
        encryption::{Duplex, split_mse},
        socket::{SocketTuning, tcp_connect},
    },
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite},
    vectored_traits::AsyncReadVectoredIntoCompat,
};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum ConnectionKind {
    #[serde(rename = "tcp")]
    Tcp,
    #[serde(rename = "utp")]
    Utp,
    /// TCP through a proxy (HTTP CONNECT, SOCKS4 or SOCKS5).
    #[serde(rename = "socks")]
    Socks,
    /// A stream through the I2P router (SAM).
    #[serde(rename = "i2p")]
    I2p,
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionKind::Tcp => f.write_str("tcp"),
            ConnectionKind::Utp => f.write_str("uTP"),
            ConnectionKind::Socks => f.write_str("socks"),
            ConnectionKind::I2p => f.write_str("i2p"),
        }
    }
}

pub struct ConnectionOptions {
    // socks5://[username:password@]host:port (also socks5h://, socks4://,
    // socks4a:// and http://). If set, all outgoing peer and tracker
    // connections go through the proxy unless `SessionOptions::net.proxy`
    // overrides it.
    pub proxy_url: Option<String>,
    // TCP outgoing connections are enabled by default
    pub enable_tcp: bool,
    pub peer_opts: Option<PeerConnectionOptions>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            enable_tcp: true,
            proxy_url: None,
            peer_opts: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct StreamConnectorArgs {
    pub settings: NetSettings,
    pub utp_socket: Option<Arc<UtpSocketUdp>>,
    pub bind_device: Option<BindDevice>,
    pub ipv4_only: bool,
}

gen_stats!(SingleStatAtomic SingleStatSnapshot, [
    attempts u64,
    successes u64,
    errors u64
], []);
gen_stats!(PerFamilyAtomic PerFamilySnapshot, [], [
    v4 SingleStatAtomic SingleStatSnapshot,
    v6 SingleStatAtomic SingleStatSnapshot
]);
gen_stats!(ConnectStatsAtomic ConnectStatsSnapshot, [], [
    socks PerFamilyAtomic PerFamilySnapshot,
    tcp PerFamilyAtomic PerFamilySnapshot,
    utp PerFamilyAtomic PerFamilySnapshot
]);

/// An established outgoing peer stream, after protocol encryption (if any).
pub(crate) struct Connected {
    pub kind: ConnectionKind,
    /// The payload is RC4-encrypted (MSE/PE).
    pub encrypted: bool,
    pub read: BoxAsyncReadVectored,
    pub write: BoxAsyncWrite,
}

/// Peers that failed the encrypted handshake are remembered (bounded) so
/// that, with encryption "enabled", they are retried in plaintext directly
/// (libtorrent's `pe_support` flag of a torrent peer).
const MAX_PLAINTEXT_PEERS: usize = 4096;

/// Outgoing peer connections. Every setting is read per connection, so
/// `Session::set_*` changes apply to the next connection attempt.
pub(crate) struct StreamConnector {
    settings: ArcSwap<NetSettings>,
    utp_socket: ArcSwapOption<UtpSocketUdp>,
    bind_device: RwLock<Option<BindDevice>>,
    stats: ConnectStatsAtomic,
    ipv4_only: bool,
    plaintext_peers: Mutex<HashSet<SocketAddr>>,
    // Our listen port as told to peers (`p`); 0 = not listening.
    listen_port: AtomicU16,
    pub(crate) i2p: crate::i2p::I2pConnector,
}

impl std::fmt::Debug for StreamConnector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamConnector")
            .field("settings", &self.settings.load())
            .finish_non_exhaustive()
    }
}

impl StreamConnector {
    pub fn new(config: StreamConnectorArgs) -> anyhow::Result<Self> {
        config.settings.validate()?;
        Ok(Self {
            settings: ArcSwap::from_pointee(config.settings),
            utp_socket: ArcSwapOption::new(config.utp_socket),
            bind_device: RwLock::new(config.bind_device),
            stats: Default::default(),
            ipv4_only: config.ipv4_only,
            plaintext_peers: Default::default(),
            listen_port: AtomicU16::new(0),
            i2p: Default::default(),
        })
    }

    pub fn settings(&self) -> Arc<NetSettings> {
        self.settings.load_full()
    }

    pub fn set_settings(&self, settings: NetSettings) {
        if settings.encryption != self.settings.load().encryption {
            self.plaintext_peers.lock().clear();
        }
        self.settings.store(Arc::new(settings));
    }

    pub fn set_advertised_listen_port(&self, port: Option<u16>) {
        self.listen_port.store(port.unwrap_or(0), Ordering::Relaxed);
    }

    /// The listen port for the extended handshake `p`, unless peers go
    /// through a proxy (incoming connections are refused then).
    pub fn advertised_listen_port(&self) -> Option<u16> {
        if self.settings.load().peer_proxy().is_some() {
            return None;
        }
        Some(self.listen_port.load(Ordering::Relaxed)).filter(|p| *p != 0)
    }

    pub fn set_utp_socket(&self, sock: Option<Arc<UtpSocketUdp>>) {
        self.utp_socket.store(sock);
    }

    pub fn set_bind_device(&self, bd: Option<BindDevice>) {
        *self.bind_device.write() = bd;
    }

    pub fn bind_device(&self) -> Option<BindDevice> {
        self.bind_device.read().clone()
    }

    pub fn tuning(settings: &NetSettings) -> SocketTuning {
        SocketTuning {
            tos: settings.peer_tos,
            send_buffer_size: settings.send_buffer_size,
            recv_buffer_size: settings.recv_buffer_size,
        }
    }

    fn get_stat(&self, kind: ConnectionKind, is_v6: bool) -> &SingleStatAtomic {
        let stat = match kind {
            ConnectionKind::Tcp => &self.stats.tcp,
            ConnectionKind::Utp => &self.stats.utp,
            ConnectionKind::Socks | ConnectionKind::I2p => &self.stats.socks,
        };
        if is_v6 { &stat.v6 } else { &stat.v4 }
    }

    async fn with_stat<R, E>(
        &self,
        kind: ConnectionKind,
        is_v6: bool,
        fut: impl Future<Output = std::result::Result<R, E>>,
    ) -> std::result::Result<R, E> {
        let stat = self.get_stat(kind, is_v6);
        stat.attempts(1);
        fut.await
            .inspect(|_| stat.successes(1))
            .inspect_err(|_| stat.errors(1))
    }

    pub fn stats(&self) -> &ConnectStatsAtomic {
        &self.stats
    }

    /// Connects to `addr` and, depending on the encryption policy, performs
    /// the MSE handshake for `info_hash`.
    ///
    /// With [`EncryptionPolicy::Enabled`] an encrypted handshake is tried
    /// first; if it fails the peer is reconnected in plaintext (libtorrent's
    /// `pe_enabled` behaviour for outgoing connections) and remembered so
    /// the next attempt is plaintext directly.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        info_hash: Id20,
        connect_timeout: Duration,
        handshake_timeout: Duration,
    ) -> Result<Connected> {
        let settings = self.settings.load_full();
        let raw = |s: Arc<NetSettings>| async move {
            match tokio::time::timeout(connect_timeout, self.connect_raw(addr, &s)).await {
                Ok(r) => r,
                Err(_) => Err(Error::Timeout("connecting")),
            }
        };
        let (kind, read, write) = raw(settings.clone()).await?;
        let policy = settings.encryption;
        let try_mse = match policy {
            EncryptionPolicy::Disabled => false,
            EncryptionPolicy::Forced => true,
            EncryptionPolicy::Enabled => !self.plaintext_peers.lock().contains(&addr),
        };
        if !try_mse {
            return Ok(Connected {
                kind,
                encrypted: false,
                read,
                write,
            });
        }
        let handshake = tokio::time::timeout(
            handshake_timeout,
            librqbit_mse::initiate(Duplex { read, write }, &info_hash.0),
        )
        .await;
        let err = match handshake {
            Ok(Ok(stream)) => {
                let (read, write, encrypted) = split_mse(stream);
                trace!(?addr, "encrypted connection established");
                return Ok(Connected {
                    kind,
                    encrypted,
                    read,
                    write,
                });
            }
            Ok(Err(e)) => Error::Mse(e),
            Err(_) => Error::Timeout("encrypted handshake"),
        };
        if policy == EncryptionPolicy::Forced {
            return Err(err);
        }
        debug!(
            ?addr,
            "encrypted handshake failed ({err:#}), retrying in plaintext"
        );
        {
            let mut g = self.plaintext_peers.lock();
            if g.len() >= MAX_PLAINTEXT_PEERS {
                g.clear();
            }
            g.insert(addr);
        }
        let (kind, read, write) = raw(settings).await?;
        Ok(Connected {
            kind,
            encrypted: false,
            read,
            write,
        })
    }

    async fn connect_raw(
        &self,
        addr: SocketAddr,
        settings: &NetSettings,
    ) -> Result<(ConnectionKind, BoxAsyncReadVectored, BoxAsyncWrite)> {
        if crate::i2p::is_i2p_addr(addr) {
            let stream = self
                .with_stat(ConnectionKind::I2p, true, self.i2p.connect(addr))
                .await?;
            let (r, w) = stream.into_split();
            return Ok((ConnectionKind::I2p, Box::new(r), Box::new(w)));
        }

        if addr.port() == 0 {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "invalid peer address (port 0): {}",
                addr
            )));
        }

        if self.ipv4_only && addr.is_ipv6() {
            return Err(Error::Anyhow(anyhow::anyhow!(
                "ipv6 disabled, skipping connection to {}",
                addr
            )));
        }

        if let Some(proxy) = settings.peer_proxy() {
            let stream = self
                .with_stat(
                    ConnectionKind::Socks,
                    addr.is_ipv6(),
                    librqbit_proxy::connect(
                        proxy,
                        librqbit_proxy::Target::Addr(addr),
                        Duration::from_secs(30),
                    ),
                )
                .await
                .map_err(Error::ProxyConnect)?;
            debug!(?addr, "connected through proxy");
            let (r, w) = stream.into_split();
            return Ok((
                ConnectionKind::Socks,
                Box::new(r.into_vectored_compat()),
                Box::new(w),
            ));
        }

        let enable_tcp = settings.enable_tcp;
        let utp_socket = if settings.enable_utp {
            self.utp_socket.load_full()
        } else {
            None
        };
        let tuning = Self::tuning(settings);
        let bind_device = self.bind_device();

        // Try to connect over TCP first. If in 1 second we haven't connected, try uTP also (if configured).
        // Whoever connects first wins.
        let tcp_connect = async {
            if !enable_tcp {
                return Ok(None);
            }
            let conn = self
                .with_stat(
                    ConnectionKind::Tcp,
                    addr.is_ipv6(),
                    tcp_connect(addr, bind_device.as_ref(), settings.outgoing_ports, &tuning),
                )
                .await?;
            debug!(?addr, "connected over TCP");
            Ok::<_, std::io::Error>(Some(conn))
        };

        let tcp_failed_notify = tokio::sync::Notify::new();

        let utp_connect = async {
            let sock = match utp_socket.as_ref() {
                Some(sock) => sock,
                None => return Ok(None),
            };

            // Give TCP priority as it's more mature and simpler.
            if enable_tcp {
                // wait until either 1 second has passed or TCP failed.
                tokio::select! {
                    _ = tcp_failed_notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }

            let conn = self
                .with_stat(ConnectionKind::Utp, addr.is_ipv6(), sock.connect(addr))
                .await?;

            debug!(?addr, "connected over uTP");
            Ok(Some(conn))
        };

        tokio::pin!(tcp_connect);
        tokio::pin!(utp_connect);

        let mut tcp_err: Option<Option<std::io::Error>> = None;
        let mut utp_err: Option<Option<librqbit_utp::Error>> = None;

        // wait until all fail, or one succeeds.
        loop {
            if let (Some(tcp), Some(utp)) = (tcp_err.as_mut(), utp_err.as_mut()) {
                match (tcp.take(), utp.take()) {
                    (Some(tcp), Some(utp)) => return Err(Error::Connect { tcp, utp }),
                    (Some(tcp), None) => return Err(Error::TcpConnect(tcp)),
                    (None, Some(utp)) => return Err(Error::UtpConnect(utp)),
                    (None, None) => return Err(Error::ConnectDisabled),
                }
            }
            tokio::select! {
                tcp_res = &mut tcp_connect, if tcp_err.is_none() => {
                    match tcp_res {
                        Ok(Some(stream)) => {
                            let (r, w) = stream.into_split();
                            return Ok((ConnectionKind::Tcp, Box::new(r), Box::new(w)));
                        },
                        Ok(None) => {
                            tcp_err = Some(None);
                            tcp_failed_notify.notify_waiters();
                        }
                        Err(e) => {
                            tcp_err = Some(Some(e));
                            tcp_failed_notify.notify_waiters();
                        }
                    }
                },
                utp_res = &mut utp_connect, if utp_err.is_none() => {
                    match utp_res {
                        Ok(Some(stream)) => {
                            let (r, w) = stream.split();
                            return Ok((ConnectionKind::Utp, Box::new(r), Box::new(w)));
                        },
                        Ok(None) => {
                            utp_err = Some(None);
                        }
                        Err(e) => {
                            utp_err = Some(Some(e));
                        }
                    }
                },
            };
        }
    }
}
