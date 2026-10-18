//! UDP tracker protocol (BEP 15) with the URL data extension (BEP 41).
//!
//! Responses are only accepted from the address the request was sent to and
//! with the transaction id we generated; anything else is dropped. Malformed
//! or truncated datagrams never panic.

use std::{
    collections::{HashMap, hash_map::Entry},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Context;
use librqbit_core::{hash_id::Id20, spawn_utils::spawn_with_cancel};
use librqbit_dualstack_sockets::{BindDevice, UdpSocket};
use parking_lot::RwLock;
use rand::RngExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, debug_span, trace, warn};

use crate::error::TrackerError;
use crate::response::{
    AnnounceResponse, MAX_PEERS_PER_RESPONSE, ScrapeData, is_usable_peer, sanitize_message,
};

const ACTION_CONNECT: u32 = 0;
const ACTION_ANNOUNCE: u32 = 1;
const ACTION_SCRAPE: u32 = 2;
const ACTION_ERROR: u32 = 3;

pub const EVENT_NONE: u32 = 0;
pub const EVENT_COMPLETED: u32 = 1;
pub const EVENT_STARTED: u32 = 2;
pub const EVENT_STOPPED: u32 = 3;

pub type ConnectionId = u64;
const CONNECTION_ID_MAGIC: ConnectionId = 0x41727101980;
// BEP 15: a connection id may be used for one minute.
const CONNECTION_ID_TTL: Duration = Duration::from_secs(60);
// Wait this long for a reply before retransmitting, then the second value.
const RETRANSMIT_TIMEOUTS: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(10)];
// Maximum URL data (BEP 41) we send.
const MAX_URL_DATA: usize = 512;

pub type TransactionId = u32;

pub fn new_transaction_id() -> TransactionId {
    rand::rng().random()
}

#[derive(Debug, Clone)]
pub struct AnnounceFields {
    pub info_hash: Id20,
    pub peer_id: Id20,
    pub downloaded: u64,
    pub left: u64,
    pub uploaded: u64,
    pub event: u32,
    /// IPv4 address to report (0 = use the source address).
    pub ip: u32,
    pub key: u32,
    /// -1 = tracker default.
    pub num_want: i32,
    pub port: u16,
    /// Path and query of the tracker URL (BEP 41), e.g. `/announce?passkey=x`.
    pub url_data: Vec<u8>,
}

#[derive(Debug)]
pub enum Request {
    Connect,
    Announce(ConnectionId, AnnounceFields),
    Scrape(ConnectionId, Id20),
}

impl Request {
    pub fn serialize(
        &self,
        transaction_id: TransactionId,
        buf: &mut [u8],
    ) -> anyhow::Result<usize> {
        struct W<'a> {
            buf: &'a mut [u8],
            offset: usize,
        }
        impl W<'_> {
            fn put(&mut self, s: &[u8]) -> anyhow::Result<()> {
                let end = self
                    .offset
                    .checked_add(s.len())
                    .filter(|end| *end <= self.buf.len())
                    .context("not enough space in buffer")?;
                self.buf[self.offset..end].copy_from_slice(s);
                self.offset = end;
                Ok(())
            }
        }

        let mut w = W { buf, offset: 0 };

        match self {
            Request::Connect => {
                w.put(&CONNECTION_ID_MAGIC.to_be_bytes())?;
                w.put(&ACTION_CONNECT.to_be_bytes())?;
                w.put(&transaction_id.to_be_bytes())?;
            }
            Request::Announce(connection_id, fields) => {
                w.put(&connection_id.to_be_bytes())?;
                w.put(&ACTION_ANNOUNCE.to_be_bytes())?;
                w.put(&transaction_id.to_be_bytes())?;
                w.put(&fields.info_hash.0)?;
                w.put(&fields.peer_id.0)?;
                w.put(&fields.downloaded.to_be_bytes())?;
                w.put(&fields.left.to_be_bytes())?;
                w.put(&fields.uploaded.to_be_bytes())?;
                w.put(&fields.event.to_be_bytes())?;
                w.put(&fields.ip.to_be_bytes())?;
                w.put(&fields.key.to_be_bytes())?;
                w.put(&fields.num_want.to_be_bytes())?;
                w.put(&fields.port.to_be_bytes())?;
                let url_data = &fields.url_data[..fields.url_data.len().min(MAX_URL_DATA)];
                if !url_data.is_empty() {
                    for chunk in url_data.chunks(255) {
                        // Option type 2 = URLData, then length, then data.
                        w.put(&[2, chunk.len() as u8])?;
                        w.put(chunk)?;
                    }
                }
            }
            Request::Scrape(connection_id, info_hash) => {
                w.put(&connection_id.to_be_bytes())?;
                w.put(&ACTION_SCRAPE.to_be_bytes())?;
                w.put(&transaction_id.to_be_bytes())?;
                w.put(&info_hash.0)?;
            }
        }
        Ok(w.offset)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct UdpAnnounceResponse {
    pub interval: u32,
    pub leechers: u32,
    pub seeders: u32,
    pub addrs: Vec<SocketAddr>,
}

impl From<UdpAnnounceResponse> for AnnounceResponse {
    fn from(r: UdpAnnounceResponse) -> Self {
        AnnounceResponse {
            interval: Some(r.interval),
            complete: Some(r.seeders),
            incomplete: Some(r.leechers),
            peers: r.addrs,
            ..Default::default()
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Response {
    Connect(ConnectionId),
    Announce(UdpAnnounceResponse),
    Scrape(ScrapeData),
    Error(String),
    Unknown,
}

fn take<const N: usize>(buf: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = buf.split_first_chunk::<N>()?;
    *buf = rest;
    Some(*head)
}

fn take_u32(buf: &mut &[u8]) -> Option<u32> {
    take::<4>(buf).map(u32::from_be_bytes)
}

impl Response {
    /// Parse a datagram. Returns `None` if it is too short to carry a header.
    /// `is_ipv6` selects the peer format of announce responses.
    pub fn parse(buf: &[u8], is_ipv6: bool) -> Option<(TransactionId, Self)> {
        let mut b = buf;
        let action = take_u32(&mut b)?;
        let tid = take_u32(&mut b)?;
        let response = Self::parse_body(action, is_ipv6, b).unwrap_or(Response::Unknown);
        Some((tid, response))
    }

    fn parse_body(action: u32, is_ipv6: bool, mut b: &[u8]) -> Option<Self> {
        match action {
            ACTION_CONNECT => Some(Response::Connect(u64::from_be_bytes(take::<8>(&mut b)?))),
            ACTION_ANNOUNCE => {
                let interval = take_u32(&mut b)?;
                let leechers = take_u32(&mut b)?;
                let seeders = take_u32(&mut b)?;
                let mut addrs = Vec::new();
                let peer_len = if is_ipv6 { 18 } else { 6 };
                for chunk in b.chunks_exact(peer_len) {
                    if addrs.len() >= MAX_PEERS_PER_RESPONSE {
                        break;
                    }
                    let (ip, port) = if is_ipv6 {
                        let mut ip = [0u8; 16];
                        ip.copy_from_slice(&chunk[..16]);
                        (IpAddr::V6(Ipv6Addr::from(ip)), [chunk[16], chunk[17]])
                    } else {
                        (
                            IpAddr::V4(Ipv4Addr::new(chunk[0], chunk[1], chunk[2], chunk[3])),
                            [chunk[4], chunk[5]],
                        )
                    };
                    let addr = SocketAddr::new(ip.to_canonical(), u16::from_be_bytes(port));
                    if is_usable_peer(&addr) {
                        addrs.push(addr);
                    }
                }
                Some(Response::Announce(UdpAnnounceResponse {
                    interval,
                    leechers,
                    seeders,
                    addrs,
                }))
            }
            ACTION_SCRAPE => {
                let seeders = take_u32(&mut b)?;
                let completed = take_u32(&mut b)?;
                let leechers = take_u32(&mut b)?;
                Some(Response::Scrape(ScrapeData {
                    complete: Some(seeders),
                    incomplete: Some(leechers),
                    downloaded: Some(completed),
                }))
            }
            ACTION_ERROR => {
                let msg = b.split(|&c| c == 0).next().unwrap_or_default();
                let msg = sanitize_message(msg);
                Some(Response::Error(if msg.is_empty() {
                    "tracker returned an error".to_owned()
                } else {
                    msg
                }))
            }
            _ => None,
        }
    }
}

fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

struct ConnectionIdMeta {
    id: ConnectionId,
    created: Instant,
}

struct PendingTransaction {
    addr: SocketAddr,
    tx: tokio::sync::oneshot::Sender<Response>,
}

#[derive(Default)]
struct ClientLocked {
    connections: HashMap<SocketAddr, ConnectionIdMeta>,
    transactions: HashMap<TransactionId, PendingTransaction>,
}

struct ClientShared {
    sock: UdpSocket,
    locked: RwLock<ClientLocked>,
}

/// A UDP tracker client sharing one socket between all torrents.
#[derive(Clone)]
pub struct UdpTrackerClient {
    state: Arc<ClientShared>,
}

struct TransactionIdGuard<'a> {
    tid: TransactionId,
    state: &'a ClientShared,
}

impl Drop for TransactionIdGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.state.locked.write();
        g.transactions.remove(&self.tid);
    }
}

impl UdpTrackerClient {
    pub async fn new(
        cancel_token: CancellationToken,
        bind_device: Option<&BindDevice>,
    ) -> anyhow::Result<Self> {
        let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let sock = UdpSocket::bind_udp(
            addr,
            librqbit_dualstack_sockets::BindOpts {
                device: bind_device,
                ..Default::default()
            },
        )
        .with_context(|| format!("error creating UDP socket at {addr}"))?;

        let client = Self {
            state: Arc::new(ClientShared {
                sock,
                locked: RwLock::new(Default::default()),
            }),
        };

        spawn_with_cancel(debug_span!("udp_tracker"), "udp_tracker", cancel_token, {
            let client = client.clone();
            async move { client.run().await }
        });

        Ok(client)
    }

    async fn run(self) -> anyhow::Result<()> {
        let mut buf = [0u8; 16384];
        loop {
            let (len, addr) = match self.state.sock.recv_from(&mut buf).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("error in UdpSocket::recv_from: {e:#}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let addr = canonical(addr);

            let Some((tid, response)) = Response::parse(&buf[..len], addr.is_ipv6()) else {
                debug!(?addr, len, "ignoring short UDP tracker datagram");
                continue;
            };

            trace!(?tid, ?addr, "received");

            let pending = {
                let mut g = self.state.locked.write();
                match g.transactions.entry(tid) {
                    Entry::Occupied(e) if e.get().addr == addr => Some(e.remove()),
                    Entry::Occupied(_) => {
                        debug!(?addr, "UDP tracker response from an unexpected address");
                        None
                    }
                    Entry::Vacant(_) => None,
                }
            };
            match pending {
                Some(p) => {
                    let _ = p.tx.send(response);
                }
                None => trace!(tid, "nowhere to send response"),
            }
        }
    }

    async fn get_connection_id(&self, addr: SocketAddr) -> Result<ConnectionId, TrackerError> {
        if let Some(m) = self.state.locked.read().connections.get(&addr)
            && m.created.elapsed() < CONNECTION_ID_TTL
        {
            return Ok(m.id);
        }

        match self.request(addr, Request::Connect).await? {
            Response::Connect(connection_id) => {
                let mut g = self.state.locked.write();
                g.connections
                    .retain(|_, m| m.created.elapsed() < CONNECTION_ID_TTL);
                g.connections.insert(
                    addr,
                    ConnectionIdMeta {
                        id: connection_id,
                        created: Instant::now(),
                    },
                );
                Ok(connection_id)
            }
            _ => Err(TrackerError::other(
                "unexpected UDP tracker response to connect",
            )),
        }
    }

    fn forget_connection_id(&self, addr: SocketAddr) {
        self.state.locked.write().connections.remove(&addr);
    }

    async fn request(&self, addr: SocketAddr, request: Request) -> Result<Response, TrackerError> {
        let addr = canonical(addr);
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let tid_g = self.reserve_transaction_id(addr, tx)?;

        let mut write_buf = [0u8; 1024];
        let len = request
            .serialize(tid_g.tid, &mut write_buf)
            .map_err(|e| TrackerError::other(format!("{e:#}")))?;

        for timeout in RETRANSMIT_TIMEOUTS {
            self.state
                .sock
                .send_to(&write_buf[..len], addr)
                .await
                .map_err(|e| TrackerError::other(format!("error sending to UDP tracker: {e}")))?;
            match tokio::time::timeout(timeout, &mut rx).await {
                Ok(Ok(response)) => {
                    return match response {
                        Response::Error(e) => Err(TrackerError::tracker_failure(e)),
                        Response::Unknown => {
                            Err(TrackerError::other("malformed UDP tracker response"))
                        }
                        r => Ok(r),
                    };
                }
                Ok(Err(_)) => return Err(TrackerError::other("UDP tracker client stopped")),
                Err(_) => continue,
            }
        }
        Err(TrackerError::other("UDP tracker timed out"))
    }

    fn reserve_transaction_id(
        &self,
        addr: SocketAddr,
        tx: tokio::sync::oneshot::Sender<Response>,
    ) -> Result<TransactionIdGuard<'_>, TrackerError> {
        let mut g = self.state.locked.write();
        let mut tx = Some(tx);
        for _ in 0..10 {
            let t = new_transaction_id();
            if let Entry::Vacant(vac) = g.transactions.entry(t) {
                vac.insert(PendingTransaction {
                    addr,
                    tx: tx.take().expect("inserted once"),
                });
                return Ok(TransactionIdGuard {
                    tid: t,
                    state: &self.state,
                });
            }
        }
        Err(TrackerError::other("can't generate a UDP transaction id"))
    }

    pub async fn announce(
        &self,
        tracker: SocketAddr,
        fields: AnnounceFields,
    ) -> Result<AnnounceResponse, TrackerError> {
        let tracker = canonical(tracker);
        let connection_id = self.get_connection_id(tracker).await?;
        let result = self
            .request(tracker, Request::Announce(connection_id, fields))
            .await;
        match result {
            Ok(Response::Announce(r)) => Ok(r.into()),
            Ok(_) => Err(TrackerError::other(
                "unexpected UDP tracker response to announce",
            )),
            Err(e) => {
                // The connection id may have been invalidated by the tracker.
                self.forget_connection_id(tracker);
                Err(e)
            }
        }
    }

    pub async fn scrape(
        &self,
        tracker: SocketAddr,
        info_hash: Id20,
    ) -> Result<ScrapeData, TrackerError> {
        let tracker = canonical(tracker);
        let connection_id = self.get_connection_id(tracker).await?;
        match self
            .request(tracker, Request::Scrape(connection_id, info_hash))
            .await
        {
            Ok(Response::Scrape(s)) => Ok(s),
            Ok(_) => Err(TrackerError::other(
                "unexpected UDP tracker response to scrape",
            )),
            Err(e) => {
                self.forget_connection_id(tracker);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_announce() {
        let b = include_bytes!("../resources/test/udp-tracker-announce-response.bin");
        let (_tid, response) = Response::parse(b, false).unwrap();
        assert!(matches!(response, Response::Announce(_)));
    }

    #[test]
    fn parse_announce_v4_and_v6() {
        let mut b = Vec::new();
        b.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        b.extend_from_slice(&7u32.to_be_bytes());
        b.extend_from_slice(&1800u32.to_be_bytes());
        b.extend_from_slice(&3u32.to_be_bytes());
        b.extend_from_slice(&9u32.to_be_bytes());
        b.extend_from_slice(&[1, 2, 3, 4, 0x1a, 0xe1]);
        b.extend_from_slice(&[0, 0, 0, 0, 0x1a, 0xe1]); // unusable
        b.extend_from_slice(&[5, 6, 7, 8, 0, 0]); // port 0
        b.extend_from_slice(&[9, 9]); // trailing garbage
        let (tid, r) = Response::parse(&b, false).unwrap();
        assert_eq!(tid, 7);
        assert_eq!(
            r,
            Response::Announce(UdpAnnounceResponse {
                interval: 1800,
                leechers: 3,
                seeders: 9,
                addrs: vec!["1.2.3.4:6881".parse().unwrap()],
            })
        );

        let mut b = Vec::new();
        b.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
        b.extend_from_slice(&8u32.to_be_bytes());
        b.extend_from_slice(&[0; 12]);
        let ip: Ipv6Addr = "2001:db8::1".parse().unwrap();
        b.extend_from_slice(&ip.octets());
        b.extend_from_slice(&[0x1a, 0xe1]);
        let (_, r) = Response::parse(&b, true).unwrap();
        match r {
            Response::Announce(a) => {
                assert_eq!(a.addrs, vec!["[2001:db8::1]:6881".parse().unwrap()])
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_error_and_scrape() {
        let mut b = Vec::new();
        b.extend_from_slice(&ACTION_ERROR.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        b.extend_from_slice(b"no such torrent\0junk");
        let (_, r) = Response::parse(&b, false).unwrap();
        assert_eq!(r, Response::Error("no such torrent".to_owned()));

        let mut b = Vec::new();
        b.extend_from_slice(&ACTION_SCRAPE.to_be_bytes());
        b.extend_from_slice(&1u32.to_be_bytes());
        for v in [10u32, 20, 30] {
            b.extend_from_slice(&v.to_be_bytes());
        }
        let (_, r) = Response::parse(&b, false).unwrap();
        assert_eq!(
            r,
            Response::Scrape(ScrapeData {
                complete: Some(10),
                downloaded: Some(20),
                incomplete: Some(30)
            })
        );
    }

    #[test]
    fn truncated_datagrams() {
        assert!(Response::parse(&[], false).is_none());
        assert!(Response::parse(&[0, 0, 0], false).is_none());
        // Header only: body unparseable.
        let (_, r) = Response::parse(&[0, 0, 0, 1, 0, 0, 0, 1], false).unwrap();
        assert_eq!(r, Response::Unknown);
        let (_, r) = Response::parse(&[0, 0, 0, 0, 0, 0, 0, 1, 1, 2], false).unwrap();
        assert_eq!(r, Response::Unknown);
        let (_, r) = Response::parse(&[0, 0, 0, 9, 0, 0, 0, 1], false).unwrap();
        assert_eq!(r, Response::Unknown);
    }

    #[test]
    fn fuzz_parse_never_panics() {
        let mut state: u64 = 0xdeadbeefcafef00d;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50_000 {
            let len = (next() % 64) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if buf.len() >= 4 {
                buf[3] = (next() % 5) as u8;
                buf[0..3].fill(0);
            }
            for v6 in [false, true] {
                if let Some((_, Response::Announce(a))) = Response::parse(&buf, v6) {
                    assert!(a.addrs.len() <= MAX_PEERS_PER_RESPONSE);
                }
            }
        }
    }

    #[test]
    fn serialize_announce_with_url_data() {
        let fields = AnnounceFields {
            info_hash: Id20::new([1; 20]),
            peer_id: Id20::new([2; 20]),
            downloaded: 1,
            left: 2,
            uploaded: 3,
            event: EVENT_STARTED,
            ip: 0,
            key: 0xabcd,
            num_want: 200,
            port: 6881,
            url_data: b"/announce?passkey=abc".to_vec(),
        };
        let mut buf = [0u8; 1024];
        let len = Request::Announce(42, fields.clone())
            .serialize(9, &mut buf)
            .unwrap();
        assert_eq!(len, 98 + 2 + 21);
        assert_eq!(&buf[96..98], &6881u16.to_be_bytes());
        assert_eq!(buf[98], 2);
        assert_eq!(buf[99], 21);
        assert_eq!(&buf[100..121], b"/announce?passkey=abc");
        assert_eq!(&buf[92..96], &200i32.to_be_bytes());

        let long = AnnounceFields {
            url_data: vec![b'a'; 4000],
            ..fields
        };
        let len = Request::Announce(42, long).serialize(9, &mut buf).unwrap();
        // 512 bytes in 3 options (255 + 255 + 2).
        assert_eq!(len, 98 + 512 + 6);
    }

    /// A local UDP tracker that answers connect and announce, and checks that
    /// responses from a wrong address or with a wrong transaction id are ignored.
    #[tokio::test]
    async fn local_udp_tracker_roundtrip() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let spoofer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let (len, from) = server.recv_from(&mut buf).await.unwrap();
                let req = &buf[..len];
                let action = u32::from_be_bytes(req[8..12].try_into().unwrap());
                let tid = &req[12..16];
                // A spoofed reply from another socket and a reply with a wrong
                // transaction id must both be dropped.
                let mut bogus = Vec::new();
                bogus.extend_from_slice(&ACTION_ERROR.to_be_bytes());
                bogus.extend_from_slice(tid);
                bogus.extend_from_slice(b"spoofed");
                spoofer.send_to(&bogus, from).await.unwrap();
                let mut wrong_tid = bogus.clone();
                wrong_tid[4] ^= 0xff;
                server.send_to(&wrong_tid, from).await.unwrap();

                let mut resp = Vec::new();
                match action {
                    ACTION_CONNECT => {
                        resp.extend_from_slice(&ACTION_CONNECT.to_be_bytes());
                        resp.extend_from_slice(tid);
                        resp.extend_from_slice(&0x1122334455667788u64.to_be_bytes());
                    }
                    ACTION_ANNOUNCE => {
                        assert_eq!(&req[0..8], &0x1122334455667788u64.to_be_bytes());
                        // BEP 41 URL data follows the 98-byte announce.
                        assert_eq!(&req[98..100], &[2, 9]);
                        assert_eq!(&req[100..109], b"/announce");
                        resp.extend_from_slice(&ACTION_ANNOUNCE.to_be_bytes());
                        resp.extend_from_slice(tid);
                        resp.extend_from_slice(&900u32.to_be_bytes());
                        resp.extend_from_slice(&2u32.to_be_bytes());
                        resp.extend_from_slice(&5u32.to_be_bytes());
                        resp.extend_from_slice(&[10, 0, 0, 1, 0x1a, 0xe1]);
                    }
                    ACTION_SCRAPE => {
                        resp.extend_from_slice(&ACTION_SCRAPE.to_be_bytes());
                        resp.extend_from_slice(tid);
                        for v in [5u32, 77, 2] {
                            resp.extend_from_slice(&v.to_be_bytes());
                        }
                    }
                    _ => continue,
                }
                server.send_to(&resp, from).await.unwrap();
            }
        });

        let client = UdpTrackerClient::new(CancellationToken::new(), None)
            .await
            .unwrap();
        let r = client
            .announce(
                server_addr,
                AnnounceFields {
                    info_hash: Id20::new([1; 20]),
                    peer_id: Id20::new([2; 20]),
                    downloaded: 0,
                    left: 10,
                    uploaded: 0,
                    event: EVENT_STARTED,
                    ip: 0,
                    key: 1,
                    num_want: 50,
                    port: 6881,
                    url_data: b"/announce".to_vec(),
                },
            )
            .await
            .unwrap();
        assert_eq!(r.interval, Some(900));
        assert_eq!(r.complete, Some(5));
        assert_eq!(r.incomplete, Some(2));
        assert_eq!(r.peers, vec!["10.0.0.1:6881".parse().unwrap()]);

        let s = client
            .scrape(server_addr, Id20::new([1; 20]))
            .await
            .unwrap();
        assert_eq!(s.downloaded, Some(77));
    }
}
