//! I2P peers through the router's SAM bridge (qBittorrent's "I2P
//! (experimental)").
//!
//! The engine's peer table is keyed by `SocketAddr`, so every I2P
//! destination gets a stable synthetic address in the IPv6 discard prefix
//! (`[100::1220:a:b:c]:1`, RFC 6666 — never a real peer). The connector sends
//! connections to such addresses through SAM (`STREAM CONNECT`), incoming
//! I2P streams (`STREAM ACCEPT`) go through the normal handshake path, and
//! I2P peers can come from `.i2p` HTTP trackers (tunnelled through SAM by
//! [`crate::i2p_http`]) or be added by destination.

use std::{
    collections::HashMap,
    net::{Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use arc_swap::ArcSwapOption;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, info, warn};

pub use librqbit_i2p::SamOptions;

use crate::{
    Error, ManagedTorrent, PeerSource, Session, stream_connect::ConnectionKind,
    torrent_state::trackers::TorrentTrackers, vectored_traits::AsyncReadVectoredIntoCompat,
};

/// Fifth 16-bit group of synthetic I2P addresses ("i2p" marker).
const I2P_MARKER: u16 = 0x1220;
/// Port of synthetic I2P addresses (peer adders skip port 0).
const I2P_PORT: u16 = 1;
/// Destinations remembered at most. Beyond this the oldest entry is
/// forgotten (destinations cost nothing to create, so a full book that
/// refused new ones would lock every new I2P peer out). Indices are never
/// reused within a session, and each session starts at a random index, so a
/// synthetic address (e.g. a persisted ban) never names another destination.
const MAX_DESTINATIONS: usize = 65_536;
/// Incoming I2P streams being checked (handshakes) at most.
const MAX_PENDING_INCOMING: usize = 64;
/// Synthetic addresses hold a 48-bit index.
const INDEX_MASK: u64 = (1 << 48) - 1;
const SAM_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// Base64 destinations are ~516+ characters; bound what we store.
const MAX_DESTINATION_LEN: usize = 1024;

/// I2P configuration (qBittorrent: I2P host/port, mixed mode, tunnel
/// quantities and lengths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2pSettings {
    pub sam: SamOptions,
    /// libtorrent `allow_i2p_mixed`: torrents with `.i2p` trackers may also
    /// use regular peers (and DHT/LSD). Off: they only talk to I2P peers.
    pub mixed_mode: bool,
}

/// State of the I2P session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct I2pStatus {
    pub enabled: bool,
    /// The SAM session is up.
    pub connected: bool,
    /// Our base64 destination.
    pub destination: Option<String>,
    /// Our `.b32.i2p` address.
    pub b32: Option<String>,
    /// Last SAM error.
    pub error: Option<String>,
}

/// An address in the IPv6 discard prefix `100::/64` (RFC 6666), where the
/// synthetic I2P peer and web seed addresses live. No real peer has one, so
/// clearnet sources (clearnet trackers, DHT, LSD, PeX) must never hand one
/// out: dialing it would open an I2P stream chosen by a clearnet party and
/// link our I2P destination to our IP address.
pub(crate) fn is_synthetic_addr(addr: SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V6(ip) => ip.segments()[..4] == [0x100, 0, 0, 0],
        std::net::IpAddr::V4(_) => false,
    }
}

pub(crate) fn is_i2p_addr(addr: SocketAddr) -> bool {
    match addr {
        SocketAddr::V6(a) => {
            let s = a.ip().segments();
            s[0] == 0x100 && s[1] == 0 && s[2] == 0 && s[3] == 0 && s[4] == I2P_MARKER
        }
        SocketAddr::V4(_) => false,
    }
}

fn addr_for_index(i: u64) -> SocketAddr {
    let ip = Ipv6Addr::new(
        0x100,
        0,
        0,
        0,
        I2P_MARKER,
        ((i >> 32) & 0xffff) as u16,
        ((i >> 16) & 0xffff) as u16,
        (i & 0xffff) as u16,
    );
    SocketAddr::new(ip.into(), I2P_PORT)
}

fn index_of_addr(addr: SocketAddr) -> Option<u64> {
    if !is_i2p_addr(addr) {
        return None;
    }
    let SocketAddr::V6(a) = addr else { return None };
    let s = a.ip().segments();
    Some((u64::from(s[5]) << 32) | (u64::from(s[6]) << 16) | u64::from(s[7]))
}

/// I2P base64 alphabet (`-` and `~` instead of `+` and `/`), plus `=`.
fn is_valid_destination(d: &str) -> bool {
    if d.is_empty() || d.len() > MAX_DESTINATION_LEN {
        return false;
    }
    if let Some(name) = d.strip_suffix(".i2p") {
        // Host names (`*.i2p`, `*.b32.i2p`): resolved through SAM.
        return !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'));
    }
    d.len() >= 387
        && d.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'~' | b'='))
}

/// Destination ↔ synthetic address.
struct AddrBook {
    by_dest: HashMap<Arc<str>, u64>,
    by_index: HashMap<u64, Arc<str>>,
    // Indices in insertion order, for eviction.
    order: std::collections::VecDeque<u64>,
    next: u64,
    capacity: usize,
}

impl Default for AddrBook {
    fn default() -> Self {
        use rand::RngExt;
        Self::with_capacity(MAX_DESTINATIONS, rand::rng().random::<u64>() & (INDEX_MASK >> 1))
    }
}

impl AddrBook {
    fn with_capacity(capacity: usize, first_index: u64) -> Self {
        Self {
            by_dest: HashMap::new(),
            by_index: HashMap::new(),
            order: Default::default(),
            next: first_index & INDEX_MASK,
            capacity,
        }
    }

    fn addr_for(&mut self, dest: &str) -> Option<SocketAddr> {
        if let Some(i) = self.by_dest.get(dest) {
            return Some(addr_for_index(*i));
        }
        if !is_valid_destination(dest) {
            return None;
        }
        while self.by_index.len() >= self.capacity {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(d) = self.by_index.remove(&old) {
                self.by_dest.remove(&d);
            }
        }
        let i = self.next;
        self.next = (self.next + 1) & INDEX_MASK;
        let d: Arc<str> = dest.into();
        self.by_index.insert(i, d.clone());
        self.by_dest.insert(d, i);
        self.order.push_back(i);
        Some(addr_for_index(i))
    }

    fn dest_of(&self, addr: SocketAddr) -> Option<Arc<str>> {
        self.by_index.get(&index_of_addr(addr)?).cloned()
    }
}

pub(crate) struct I2pRuntime {
    pub session: librqbit_i2p::Session,
}

impl I2pRuntime {
    /// Base64 destination for a destination or `*.i2p` name.
    pub async fn resolve(&self, dest: &str) -> crate::Result<String> {
        if dest.ends_with(".i2p") {
            return tokio::time::timeout(SAM_TIMEOUT, self.session.lookup(dest))
                .await
                .map_err(|_| Error::Timeout("I2P name lookup"))?
                .map_err(|e| Error::Anyhow(e.into()));
        }
        Ok(dest.to_owned())
    }

    pub async fn connect(&self, dest: &str) -> crate::Result<TcpStream> {
        let dest = self.resolve(dest).await?;
        self.session
            .connect(&dest, CONNECT_TIMEOUT)
            .await
            .map_err(|e| Error::Anyhow(e.into()))
    }
}

/// Outgoing I2P connections (part of the stream connector).
#[derive(Default)]
pub(crate) struct I2pConnector {
    pub runtime: ArcSwapOption<I2pRuntime>,
    book: Mutex<AddrBook>,
}

impl I2pConnector {
    pub fn addr_for(&self, dest: &str) -> Option<SocketAddr> {
        self.book.lock().addr_for(dest)
    }

    pub fn destination_of(&self, addr: SocketAddr) -> Option<Arc<str>> {
        self.book.lock().dest_of(addr)
    }

    pub async fn connect(&self, addr: SocketAddr) -> crate::Result<TcpStream> {
        let runtime = self
            .runtime
            .load_full()
            .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("I2P is not enabled")))?;
        let dest = self
            .destination_of(addr)
            .ok_or_else(|| Error::Anyhow(anyhow::anyhow!("unknown I2P peer {addr}")))?;
        runtime.connect(&dest).await
    }
}

/// Session-side I2P control: settings, the SAM session task and status.
#[derive(Default)]
pub(crate) struct I2pSessionCtl {
    settings: RwLock<Option<I2pSettings>>,
    token: Mutex<Option<CancellationToken>>,
    proxy_token: Mutex<Option<CancellationToken>>,
    status: Arc<RwLock<I2pStatus>>,
}

/// Whether a torrent announces to I2P trackers.
pub(crate) fn is_i2p_torrent(tm: &TorrentTrackers) -> bool {
    tm.entries().iter().any(|(u, _)| is_i2p_url(u))
}

pub(crate) fn is_i2p_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.to_ascii_lowercase().ends_with(".i2p"))
        })
        .unwrap_or(false)
}

impl I2pSessionCtl {
    pub fn set_proxy_token(&self, token: Option<CancellationToken>) {
        if let Some(old) = std::mem::replace(&mut *self.proxy_token.lock(), token) {
            old.cancel();
        }
    }

    pub fn settings(&self) -> Option<I2pSettings> {
        self.settings.read().clone()
    }

    /// With I2P on and mixed mode off, I2P torrents only use I2P peers (and
    /// no DHT/LSD).
    pub fn i2p_only(&self, tm: &TorrentTrackers) -> bool {
        self.settings.read().as_ref().is_some_and(|s| !s.mixed_mode) && is_i2p_torrent(tm)
    }

    pub fn mixed_allows(&self, torrent: &ManagedTorrent, addr: SocketAddr) -> bool {
        self.mixed_allows_trackers(&torrent.shared().tracker_manager, addr)
    }

    pub fn mixed_allows_trackers(&self, tm: &TorrentTrackers, addr: SocketAddr) -> bool {
        is_i2p_addr(addr) || !self.i2p_only(tm)
    }
}

impl Session {
    /// Start, reconfigure or stop (`None`) I2P. The SAM session is created in
    /// the background (retried with backoff while it fails); see
    /// [`Session::i2p_status`].
    pub fn set_i2p(self: &Arc<Self>, settings: Option<I2pSettings>) {
        let ctl = &self.i2p;
        if *ctl.settings.read() == settings {
            return;
        }
        if let Some(t) = ctl.token.lock().take() {
            t.cancel();
        }
        self.connector.i2p.runtime.store(None);
        *ctl.settings.write() = settings.clone();
        *ctl.status.write() = I2pStatus {
            enabled: settings.is_some(),
            ..Default::default()
        };
        self.stop_i2p_http_proxy();
        let Some(settings) = settings else {
            info!("I2P disabled");
            return;
        };
        let token = self.cancellation_token().child_token();
        *ctl.token.lock() = Some(token.clone());
        let session = Arc::downgrade(self);
        let status = ctl.status.clone();
        librqbit_core::spawn_utils::spawn_with_cancel(
            debug_span!(parent: self.rs(), "i2p"),
            "i2p",
            token,
            async move {
                let mut backoff = Duration::from_secs(5);
                loop {
                    match librqbit_i2p::Session::create(settings.sam.clone(), SAM_TIMEOUT).await {
                        Ok(sam) => {
                            backoff = Duration::from_secs(5);
                            info!(b32 = ?sam.b32(), "I2P session established");
                            let (b32, destination) = (sam.b32(), sam.destination.clone());
                            let runtime = Arc::new(I2pRuntime { session: sam });
                            let Some(s) = session.upgrade() else {
                                return Ok::<_, anyhow::Error>(());
                            };
                            s.connector.i2p.runtime.store(Some(runtime.clone()));
                            if let Err(e) = s.start_i2p_http_proxy(runtime.clone()).await {
                                warn!("error starting the I2P tracker proxy: {e:#}");
                            }
                            drop(s);
                            {
                                let mut st = status.write();
                                st.connected = true;
                                st.error = None;
                                st.b32 = b32;
                                st.destination = Some(destination);
                            }
                            let e = accept_loop(&session, &runtime).await;
                            warn!("I2P session lost: {e:#}");
                            if let Some(s) = session.upgrade() {
                                s.connector.i2p.runtime.store(None);
                                s.stop_i2p_http_proxy();
                            }
                            let mut st = status.write();
                            st.connected = false;
                            st.error = Some(format!("{e:#}"));
                        }
                        Err(e) => {
                            debug!("error creating I2P session: {e}");
                            let mut st = status.write();
                            st.connected = false;
                            st.error = Some(e.to_string());
                        }
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(300));
                }
            },
        );
    }

    /// The I2P settings (`None` = off).
    pub fn i2p_settings(&self) -> Option<I2pSettings> {
        self.i2p.settings()
    }

    pub fn i2p_status(&self) -> I2pStatus {
        self.i2p.status.read().clone()
    }

    /// Add I2P peers (base64 destinations or `*.i2p` / `*.b32.i2p` names) to a
    /// torrent. Returns how many were new. Invalid destinations are skipped.
    pub fn add_i2p_peers(&self, torrent: &ManagedTorrent, destinations: &[String]) -> usize {
        destinations
            .iter()
            .filter_map(|d| self.connector.i2p.addr_for(d.trim()))
            .filter(|a| torrent.add_peer_with_source(*a, PeerSource::Manual))
            .count()
    }

    /// The synthetic peer address of an I2P destination (as listed in peer
    /// details).
    pub fn i2p_peer_addr(&self, destination: &str) -> Option<SocketAddr> {
        self.connector.i2p.addr_for(destination)
    }

    /// The destination behind a synthetic I2P peer address.
    pub fn i2p_destination_of(&self, addr: SocketAddr) -> Option<String> {
        self.connector
            .i2p
            .destination_of(addr)
            .map(|d| d.to_string())
    }
}

/// Accepts incoming I2P streams until the SAM session fails.
async fn accept_loop(session: &std::sync::Weak<Session>, runtime: &Arc<I2pRuntime>) -> Error {
    let mut failures = 0u32;
    let pending = Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_INCOMING));
    loop {
        let accepted = runtime.session.accept_with_prefix().await;
        let librqbit_i2p::AcceptedStream {
            stream,
            destination: dest,
            prefix,
        } = match accepted {
            Ok(v) => {
                failures = 0;
                v
            }
            Err(e) => {
                failures += 1;
                if failures >= 3 {
                    return Error::Anyhow(e.into());
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let Some(s) = session.upgrade() else {
            return Error::SessionDestroyed;
        };
        // Bounded like the TCP/uTP listeners: a flood of streams (from
        // throwaway destinations) can't pile up handshake tasks.
        let Ok(permit) = pending.clone().try_acquire_owned() else {
            debug!("too many pending incoming I2P connections, dropping one");
            continue;
        };
        let Some(addr) = s.connector.i2p.addr_for(&dest) else {
            debug!("ignoring incoming I2P stream with an invalid destination");
            continue;
        };
        if !s.accept_incoming_allowed(addr) {
            continue;
        }
        let (r, w) = stream.into_split();
        let reader: crate::type_aliases::BoxAsyncReadVectored = if prefix.is_empty() {
            Box::new(r.into_vectored_compat())
        } else {
            Box::new(crate::net::encryption::MseReader::new(
                r.into_vectored_compat(),
                None,
                prefix,
            ))
        };
        let fut =
            s.clone()
                .check_incoming_connection(addr, ConnectionKind::I2p, reader, Box::new(w));
        librqbit_core::spawn_utils::spawn(
            debug_span!(parent: s.rs(), "i2p_incoming", %addr),
            "i2p_incoming",
            async move {
                let res = fut.await;
                drop(permit);
                match res {
                    Ok((live, checked)) => {
                        if let Err(e) = live.add_incoming_peer(checked) {
                            debug!("error handing over incoming I2P connection: {e:#}");
                        }
                    }
                    Err(e) => debug!("error checking incoming I2P connection: {e:#}"),
                }
                Ok::<_, anyhow::Error>(())
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(c: char) -> String {
        std::iter::repeat_n(c, 516).collect::<String>() + "AAAA"
    }

    #[test]
    fn address_book_is_bounded_and_never_reuses_addresses() {
        let mut book = AddrBook::with_capacity(2, INDEX_MASK);
        let a = book.addr_for(&dest('A')).unwrap();
        let b = book.addr_for(&dest('B')).unwrap();
        // Full: the oldest destination is forgotten instead of refusing new
        // ones forever.
        let c = book.addr_for(&dest('C')).unwrap();
        assert_eq!(book.dest_of(a), None);
        assert_eq!(book.dest_of(b).as_deref(), Some(dest('B').as_str()));
        assert_eq!(book.dest_of(c).as_deref(), Some(dest('C').as_str()));
        // A returning destination gets a new address; the old one stays dead.
        let a2 = book.addr_for(&dest('A')).unwrap();
        assert!(a2 != a && a2 != b && a2 != c);
        assert_eq!(book.dest_of(a), None);
        assert_eq!(book.by_index.len(), 2);
    }

    #[test]
    fn synthetic_addresses() {
        let mut book = AddrBook::default();
        let a = book.addr_for(&dest('A')).unwrap();
        let b = book.addr_for(&dest('B')).unwrap();
        assert_ne!(a, b);
        assert_eq!(book.addr_for(&dest('A')), Some(a));
        assert!(is_i2p_addr(a) && is_i2p_addr(b));
        assert_eq!(a.port(), I2P_PORT);
        assert_eq!(book.dest_of(b).as_deref(), Some(dest('B').as_str()));
        assert_eq!(book.dest_of("[100::1220:0:0:99]:1".parse().unwrap()), None);
        assert!(!is_i2p_addr("1.2.3.4:5".parse().unwrap()));
        // Sessions start at a random index; an address never names a
        // destination it didn't name before (no reuse).
        assert_eq!(index_of_addr(addr_for_index(INDEX_MASK)), Some(INDEX_MASK));
        assert!(is_synthetic_addr(a) && is_synthetic_addr("[100::7765:6273:0:1]:0".parse().unwrap()));
        assert!(!is_synthetic_addr("[2001:db8::1]:5".parse().unwrap()));
        assert!(!is_synthetic_addr("1.2.3.4:5".parse().unwrap()));
        assert!(!is_i2p_addr("[100::7765:6273:0:1]:0".parse().unwrap()));
        assert_eq!(
            index_of_addr(addr_for_index(0x1234_5678_9abc)),
            Some(0x1234_5678_9abc)
        );

        // Names are accepted (resolved through SAM), garbage is not.
        assert!(book.addr_for("tracker2.postman.i2p").is_some());
        assert!(
            book.addr_for("abcdefghijklmnopqrstuvwxyz234567abcdefghijklmnopqrst.b32.i2p")
                .is_some()
        );
        assert!(book.addr_for("x y").is_none());
        assert!(book.addr_for("short").is_none());
        assert!(book.addr_for(".i2p").is_none());
        assert!(book.addr_for(&"A".repeat(2000)).is_none());
        assert!(book.addr_for("evil\n.i2p").is_none());
    }

    #[test]
    fn i2p_urls() {
        assert!(is_i2p_url("http://tracker2.postman.i2p/announce.php"));
        assert!(is_i2p_url("http://ABC.B32.I2P/a"));
        assert!(!is_i2p_url("http://example.com/announce"));
        assert!(!is_i2p_url("udp://x.i2p.example.com:1/announce"));
        assert!(!is_i2p_url("garbage"));
    }
}
