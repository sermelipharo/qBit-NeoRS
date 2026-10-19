// End-to-end checks of engine-net over real local connections: protocol
// encryption, an HTTP CONNECT proxy, anonymous mode, live listen port
// changes, PeX switching, the peer IP filter and NAT-PMP port mapping.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
};

use crate::{
    AddTorrent, AddTorrentOptions, BlockedPeer, ConnectionKind, EncryptionPolicy, ListenerMode,
    ManagedTorrent, MappingProtocol, MappingTransport, NetSettings, PeerBlockReason, PeerSource,
    PortMappingConfig, ProxyConfig, ProxyKind, ProxySettings, Session, SessionOptions,
    create_torrent,
    limits::LimitsConfig,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    tests::test_util::{create_default_random_dir_with_torrents, setup_test_logging, wait_until},
};

async fn make_torrent(file_size: usize) -> (tempfile::TempDir, Bytes) {
    let dir = create_default_random_dir_with_torrents(2, file_size, Some("rqbit_net"));
    let torrent = create_torrent(
        dir.path(),
        crate::CreateTorrentOptions {
            piece_length: Some(32768),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let bytes = torrent.as_bytes().unwrap();
    (dir, bytes)
}

fn listener() -> ListenerOptions {
    ListenerOptions {
        mode: ListenerMode::TcpOnly,
        listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
        ..Default::default()
    }
}

async fn session(out: &Path, listen: bool, net: NetSettings) -> anyhow::Result<Arc<Session>> {
    Session::new_with_opts(
        out.to_owned(),
        SessionOptions {
            dht: None,
            listen: listen.then(listener),
            disable_local_service_discovery: true,
            net,
            ..Default::default()
        },
    )
    .await
}

/// One host holding silent handshakes can't take every incoming slot: its
/// connections above the per-address cap are closed at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_e2e_net_pending_handshakes_per_address() {
    setup_test_logging();
    let dir = tempfile::TempDir::with_prefix("rqbit_pending").unwrap();
    let s = session(dir.path(), true, NetSettings::default()).await.unwrap();
    let port = s.listen_addr().unwrap().port();
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    let mut silent = Vec::new();
    for _ in 0..crate::session::MAX_PENDING_HANDSHAKES_PER_IP {
        silent.push(TcpStream::connect(target).await.unwrap());
        // Let the listener register it before the next one.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut extra = TcpStream::connect(target).await.unwrap();
    let mut buf = [0u8; 1];
    let closed = tokio::time::timeout(Duration::from_secs(5), extra.read(&mut buf)).await;
    assert!(
        matches!(closed, Ok(Ok(0)) | Ok(Err(_))),
        "the connection over the cap should be closed: {closed:?}"
    );
    // The ones within the cap are still waiting for their handshake.
    let open = tokio::time::timeout(Duration::from_millis(500), silent[0].read(&mut buf)).await;
    assert!(open.is_err(), "a pending handshake was closed early: {open:?}");
    s.stop().await;
}

async fn add_seed(
    session: &Arc<Session>,
    torrent: Bytes,
    dir: &Path,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(dir.to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    wait_until(
        || {
            let s = handle.stats();
            ensure!(s.live.is_some() && s.finished, "seeder not live yet");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await?;
    Ok(handle)
}

async fn add_leech(
    session: &Arc<Session>,
    torrent: Bytes,
    peers: Vec<SocketAddr>,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                initial_peers: Some(peers),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")
}

async fn wait_finished(handle: &ManagedTorrent, secs: u64) -> anyhow::Result<()> {
    wait_until(
        || {
            let s = handle.stats();
            if let Some(e) = s.error {
                bail!("torrent errored: {e}");
            }
            ensure!(s.live.is_some() && s.finished, "not finished");
            Ok(())
        },
        Duration::from_secs(secs),
    )
    .await
}

fn net(encryption: EncryptionPolicy) -> NetSettings {
    NetSettings {
        encryption,
        ..Default::default()
    }
}

struct Pair {
    _seed_dir: tempfile::TempDir,
    _leech_dir: tempfile::TempDir,
    seeder: Arc<Session>,
    seed: Arc<ManagedTorrent>,
    leecher: Arc<Session>,
    leech: Arc<ManagedTorrent>,
}

impl Pair {
    async fn stop(self) {
        self.leecher.stop().await;
        self.seeder.stop().await;
    }
}

/// A seeder and a leecher that connects to it. `throttle` slows the upload
/// so the connection can be observed while downloading.
async fn pair(
    seeder_net: NetSettings,
    leecher_net: NetSettings,
    throttle: bool,
) -> anyhow::Result<Pair> {
    let (seed_dir, torrent) = make_torrent(300_000).await;
    let leech_dir = tempfile::TempDir::with_prefix("rqbit_net_leech")?;
    let seeder = session(seed_dir.path(), true, seeder_net).await?;
    let seed = add_seed(&seeder, torrent.clone(), seed_dir.path()).await?;
    if throttle {
        seed.set_ratelimits(LimitsConfig {
            upload_bps: std::num::NonZeroU32::new(60_000),
            download_bps: None,
        });
    }
    let addr = seeder.listen_addr().context("not listening")?;
    let leecher = session(leech_dir.path(), false, leecher_net).await?;
    let leech = add_leech(&leecher, torrent, vec![addr]).await?;
    Ok(Pair {
        _seed_dir: seed_dir,
        _leech_dir: leech_dir,
        seeder,
        seed,
        leecher,
        leech,
    })
}

async fn wait_connected(handle: &ManagedTorrent) -> anyhow::Result<()> {
    wait_until(
        || {
            ensure!(!handle.peers_detailed().is_empty(), "no peers");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_encryption_forced_both_sides() {
    setup_test_logging();
    let p = pair(
        net(EncryptionPolicy::Forced),
        net(EncryptionPolicy::Forced),
        true,
    )
    .await
    .unwrap();
    wait_connected(&p.leech).await.unwrap();
    wait_connected(&p.seed).await.unwrap();
    let lp = p.leech.peers_detailed();
    assert!(lp[0].flags.encrypted, "{:?}", lp[0].flags);
    assert!(!lp[0].flags.encrypted_handshake);
    let sp = p.seed.peers_detailed();
    assert!(sp[0].flags.encrypted && sp[0].flags.incoming);
    p.seed.set_ratelimits(LimitsConfig::default());
    wait_finished(&p.leech, 60).await.unwrap();
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_encryption_enabled_disabled_interop() {
    setup_test_logging();
    // Leecher tries MSE, the plaintext-only seeder drops it, the leecher
    // retries in plaintext.
    let p = pair(
        net(EncryptionPolicy::Disabled),
        net(EncryptionPolicy::Enabled),
        false,
    )
    .await
    .unwrap();
    wait_finished(&p.leech, 60).await.unwrap();
    p.stop().await;

    // A plaintext leecher is accepted by a seeder that allows encryption.
    let p = pair(
        net(EncryptionPolicy::Enabled),
        net(EncryptionPolicy::Disabled),
        true,
    )
    .await
    .unwrap();
    wait_connected(&p.leech).await.unwrap();
    assert!(!p.leech.peers_detailed()[0].flags.encrypted);
    p.seed.set_ratelimits(LimitsConfig::default());
    wait_finished(&p.leech, 60).await.unwrap();
    p.stop().await;

    // Both allowing: encrypted.
    let p = pair(
        net(EncryptionPolicy::Enabled),
        net(EncryptionPolicy::Enabled),
        true,
    )
    .await
    .unwrap();
    wait_connected(&p.leech).await.unwrap();
    assert!(p.leech.peers_detailed()[0].flags.encrypted);
    p.stop().await;

    // Forced vs disabled never connect.
    let p = pair(
        net(EncryptionPolicy::Forced),
        net(EncryptionPolicy::Disabled),
        false,
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(p.leech.peers_detailed().is_empty());
    assert!(!p.leech.stats().finished);
    // Switching the leecher at runtime lets new connections through.
    p.leecher
        .set_encryption_policy(EncryptionPolicy::Enabled)
        .unwrap();
    assert_eq!(
        p.leecher.net_settings().encryption,
        EncryptionPolicy::Enabled
    );
    let dir = tempfile::TempDir::with_prefix("rqbit_net_leech3").unwrap();
    let torrent = p.seed.with_metadata(|m| m.torrent_bytes.clone()).unwrap();
    let l3 = session(dir.path(), false, net(EncryptionPolicy::Disabled))
        .await
        .unwrap();
    l3.set_encryption_policy(EncryptionPolicy::Forced).unwrap();
    let h3 = add_leech(&l3, torrent, vec![p.seeder.listen_addr().unwrap()])
        .await
        .unwrap();
    wait_finished(&h3, 60).await.unwrap();
    assert!(h3.peers_detailed().iter().all(|p| p.flags.encrypted));
    l3.stop().await;
    p.stop().await;
}

/// A minimal HTTP CONNECT proxy; counts tunnels.
async fn connect_proxy() -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let c = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let c = c.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut b).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let target = head
                    .strip_prefix("CONNECT ")
                    .and_then(|r| r.split_whitespace().next())
                    .unwrap_or_default()
                    .to_owned();
                let Ok(mut upstream) = TcpStream::connect(&target).await else {
                    let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                    return;
                };
                c.fetch_add(1, Ordering::SeqCst);
                s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut s, &mut upstream).await;
            });
        }
    });
    (addr, count)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_http_connect_proxy() {
    setup_test_logging();
    let (proxy, tunnels) = connect_proxy().await;
    let leecher_net = NetSettings {
        proxy: Some(ProxySettings {
            config: ProxyConfig {
                kind: ProxyKind::Http,
                host: proxy.ip().to_string(),
                port: proxy.port(),
                auth: None,
                remote_dns: true,
            },
            peer_connections: true,
            tracker_connections: false,
        }),
        ..Default::default()
    };
    let p = pair(NetSettings::default(), leecher_net, true)
        .await
        .unwrap();
    wait_connected(&p.leech).await.unwrap();
    let peer = &p.leech.peers_detailed()[0];
    assert!(matches!(peer.connection, ConnectionKind::Socks));
    // Encryption is negotiated through the tunnel too.
    assert!(peer.flags.encrypted);
    assert!(tunnels.load(Ordering::SeqCst) >= 1);
    p.seed.set_ratelimits(LimitsConfig::default());
    wait_finished(&p.leech, 60).await.unwrap();

    // Turning the proxy off at runtime: direct connections again.
    p.leecher.set_proxy(None).unwrap();
    assert!(p.leecher.net_settings().proxy.is_none());
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_anonymous_mode() {
    setup_test_logging();
    let p = pair(
        NetSettings::default(),
        NetSettings {
            anonymous_mode: true,
            ..Default::default()
        },
        true,
    )
    .await
    .unwrap();
    wait_connected(&p.seed).await.unwrap();
    let sp = &p.seed.peers_detailed()[0];
    assert!(sp.peer_id.starts_with(b"-LT2000-"), "{:?}", sp.peer_id);
    // No `v` in the extended handshake: the name comes from the peer id.
    assert_ne!(sp.client, crate::client_name_and_version());
    assert!(p.leecher.tracker_settings().anonymous_mode);

    // Off again at runtime: new connections use the regular id.
    p.leecher.set_anonymous_mode(false).unwrap();
    assert!(!p.leecher.tracker_settings().anonymous_mode);
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_live_listen_port_change() {
    setup_test_logging();
    let p = pair(NetSettings::default(), NetSettings::default(), false)
        .await
        .unwrap();
    wait_finished(&p.leech, 60).await.unwrap();
    let old = p.seeder.listen_addr().unwrap();

    let new_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    p.seeder.set_listen_port(new_port).await.unwrap();
    let new = p.seeder.listen_addr().unwrap();
    assert_eq!(new.port(), new_port);
    assert_eq!(new.ip(), old.ip());
    // The old port is closed.
    wait_until(
        || {
            ensure!(
                std::net::TcpStream::connect_timeout(&old, Duration::from_millis(200)).is_err(),
                "old port still open"
            );
            Ok(())
        },
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // A new leecher downloads from the new port.
    let dir = tempfile::TempDir::with_prefix("rqbit_net_leech2").unwrap();
    let l2 = session(dir.path(), false, NetSettings::default())
        .await
        .unwrap();
    let torrent = p.seed.with_metadata(|m| m.torrent_bytes.clone()).unwrap();
    let h2 = add_leech(&l2, torrent, vec![new]).await.unwrap();
    wait_finished(&h2, 60).await.unwrap();

    // Switching to TCP+uTP on the same port.
    let mut opts = p.seeder.listen_options().unwrap();
    opts.mode = ListenerMode::TcpAndUtp;
    p.seeder.set_listen_options(Some(opts)).await.unwrap();
    assert_eq!(p.seeder.listen_addr().unwrap().port(), new_port);

    // Not listening at all.
    p.seeder.set_listen_options(None).await.unwrap();
    assert!(p.seeder.listen_addr().is_none());
    l2.stop().await;
    p.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_pex_toggle() {
    setup_test_logging();
    // S seeds, B is a listening leecher with no peers, A connects out to both
    // and tells B about S over PeX (only when PeX is on).
    let (seed_dir, torrent) = make_torrent(300_000).await;
    let s = session(seed_dir.path(), true, NetSettings::default())
        .await
        .unwrap();
    let seed = add_seed(&s, torrent.clone(), seed_dir.path())
        .await
        .unwrap();
    seed.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(20_000),
        download_bps: None,
    });
    let s_addr = s.listen_addr().unwrap();

    let b_dir = tempfile::TempDir::with_prefix("rqbit_net_b").unwrap();
    let b = session(b_dir.path(), true, NetSettings::default())
        .await
        .unwrap();
    let bt = add_leech(&b, torrent.clone(), vec![]).await.unwrap();
    let b_addr = b.listen_addr().unwrap();
    wait_until(
        || {
            ensure!(bt.stats().live.is_some(), "B not live");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await
    .unwrap();

    let a_dir = tempfile::TempDir::with_prefix("rqbit_net_a").unwrap();
    let a = session(
        a_dir.path(),
        false,
        NetSettings {
            pex: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let at = add_leech(&a, torrent, vec![s_addr, b_addr]).await.unwrap();
    wait_until(
        || {
            ensure!(at.peers_detailed().len() == 2, "A not connected to both");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await
    .unwrap();

    let b_knows_s = || {
        bt.peers_detailed()
            .iter()
            .any(|p| p.addr == s_addr && p.sources.contains(PeerSource::Pex))
    };
    // PeX off: nothing within several PeX intervals (2 s in tests).
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!b_knows_s(), "B learned about S while PeX was off");

    // On at runtime: B learns S through A and connects.
    a.set_pex_enabled(true).unwrap();
    wait_until(
        || {
            ensure!(b_knows_s(), "B doesn't know S yet");
            Ok(())
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap();

    a.stop().await;
    b.stop().await;
    s.stop().await;
}

/// The same torrent with `private=1` added to its info dict (BEP 27).
fn make_private(torrent: &Bytes) -> Bytes {
    // "private" sorts after "pieces", the last key of the info dict, and
    // "info" is the last key of the torrent.
    assert!(torrent.ends_with(b"ee"));
    let mut v = torrent[..torrent.len() - 2].to_vec();
    v.extend_from_slice(b"7:privatei1eee");
    let b = Bytes::from(v);
    let parsed = crate::torrent_from_bytes(&b).unwrap();
    assert!(parsed.info.data.private);
    b
}

/// S seeds, B is a listening leecher with no peers, A connects out to both
/// and may tell B about S over PeX (on in every session).
struct PexTrio {
    s: Arc<Session>,
    a: Arc<Session>,
    b: Arc<Session>,
    at: Arc<ManagedTorrent>,
    bt: Arc<ManagedTorrent>,
    s_addr: SocketAddr,
    _dirs: Vec<tempfile::TempDir>,
}

impl PexTrio {
    async fn new(torrent: Bytes, seed_dir: tempfile::TempDir, a_opts: AddTorrentOptions) -> Self {
        let s = session(seed_dir.path(), true, NetSettings::default())
            .await
            .unwrap();
        let seed = add_seed(&s, torrent.clone(), seed_dir.path())
            .await
            .unwrap();
        seed.set_ratelimits(LimitsConfig {
            upload_bps: std::num::NonZeroU32::new(20_000),
            download_bps: None,
        });
        let s_addr = s.listen_addr().unwrap();
        let b_dir = tempfile::TempDir::with_prefix("rqbit_net_b").unwrap();
        let b = session(b_dir.path(), true, NetSettings::default())
            .await
            .unwrap();
        let bt = add_leech(&b, torrent.clone(), vec![]).await.unwrap();
        let b_addr = b.listen_addr().unwrap();
        wait_until(
            || {
                ensure!(bt.stats().live.is_some(), "B not live");
                Ok(())
            },
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        let a_dir = tempfile::TempDir::with_prefix("rqbit_net_a").unwrap();
        let a = session(a_dir.path(), false, NetSettings::default())
            .await
            .unwrap();
        let at = a
            .add_torrent(
                AddTorrent::TorrentFileBytes(torrent),
                Some(AddTorrentOptions {
                    initial_peers: Some(vec![s_addr, b_addr]),
                    ..a_opts
                }),
            )
            .await
            .unwrap()
            .into_handle()
            .unwrap();
        wait_until(
            || {
                ensure!(at.peers_detailed().len() == 2, "A not connected to both");
                Ok(())
            },
            Duration::from_secs(20),
        )
        .await
        .unwrap();
        Self {
            s,
            a,
            b,
            at,
            bt,
            s_addr,
            _dirs: vec![seed_dir, b_dir, a_dir],
        }
    }

    fn b_knows_s(&self) -> bool {
        self.bt
            .peers_detailed()
            .iter()
            .any(|p| p.addr == self.s_addr && p.sources.contains(PeerSource::Pex))
    }

    async fn stop(self) {
        self.a.stop().await;
        self.b.stop().await;
        self.s.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_pex_disabled_for_torrent() {
    setup_test_logging();
    let (seed_dir, torrent) = make_torrent(300_000).await;
    let t = PexTrio::new(
        torrent,
        seed_dir,
        AddTorrentOptions {
            disabled_discovery: crate::DisabledDiscovery {
                pex: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert!(t.at.disabled_discovery().pex);
    // PeX off for A's torrent: nothing within several PeX intervals (2 s in
    // tests) although the session has PeX on.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!t.b_knows_s(), "B learned about S while PeX was off for A's torrent");

    // On at runtime: B learns S through A.
    t.at.set_disabled_discovery(crate::DisabledDiscovery::default());
    wait_until(
        || {
            ensure!(t.b_knows_s(), "B doesn't know S yet");
            Ok(())
        },
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_private_torrent_never_uses_pex() {
    setup_test_logging();
    let (seed_dir, torrent) = make_torrent(300_000).await;
    let torrent = make_private(&torrent);
    let t = PexTrio::new(torrent, seed_dir, AddTorrentOptions::default()).await;
    assert!(t.at.is_private());
    // Session PeX on everywhere, no per-torrent switch: still nothing.
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!t.b_knows_s(), "PeX was used for a private torrent");
    t.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_ip_filter_disconnects() {
    setup_test_logging();
    let p = pair(NetSettings::default(), NetSettings::default(), true)
        .await
        .unwrap();
    wait_connected(&p.seed).await.unwrap();

    let blocked: Arc<Mutex<Vec<BlockedPeer>>> = Default::default();
    let b2 = blocked.clone();
    p.seeder
        .set_blocked_peer_callback(Some(Arc::new(move |b| b2.lock().push(b))));
    p.seeder.set_peer_ip_filter(Some(Arc::new(|ip: IpAddr| {
        ip == IpAddr::V4(Ipv4Addr::LOCALHOST)
    })));
    wait_until(
        || {
            ensure!(p.seed.peers_detailed().is_empty(), "still connected");
            Ok(())
        },
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    {
        let b = blocked.lock();
        let first = b.first().expect("disconnect not reported");
        assert_eq!(first.reason, PeerBlockReason::IpFilter);
        assert!(first.incoming);
        assert_eq!(first.addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    // Reconnects are refused (and reported) at accept time.
    let n = blocked.lock().len();
    let addr = p.seeder.listen_addr().unwrap();
    p.leech.add_peer_with_source(addr, PeerSource::Manual);
    let mut s = TcpStream::connect(addr).await.unwrap();
    let mut buf = [0u8; 1];
    let r = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await;
    assert!(matches!(r, Ok(Ok(0)) | Ok(Err(_))), "{r:?}");
    assert!(blocked.lock().len() > n);

    // Bans work the same way; lifting the filter lets peers in again.
    p.seeder.set_peer_ip_filter(None);
    p.seeder.ban_peer_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(p.seeder.banned_ips(), vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    let n = blocked.lock().len();
    let mut s = TcpStream::connect(addr).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await;
    assert!(matches!(r, Ok(Ok(0)) | Ok(Err(_))), "{r:?}");
    assert_eq!(blocked.lock()[n].reason, PeerBlockReason::Banned);

    // Outgoing connections are filtered on the leecher side too.
    let lblocked: Arc<Mutex<Vec<BlockedPeer>>> = Default::default();
    let l2 = lblocked.clone();
    p.leecher
        .set_blocked_peer_callback(Some(Arc::new(move |b| l2.lock().push(b))));
    p.leecher
        .set_banned_ips(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    let other: SocketAddr = (Ipv4Addr::LOCALHOST, 1).into();
    p.leech.add_peer_with_source(other, PeerSource::Manual);
    wait_until(
        || {
            ensure!(
                lblocked
                    .lock()
                    .iter()
                    .any(|b| b.addr == other && !b.incoming),
                "outgoing block not reported"
            );
            Ok(())
        },
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    p.stop().await;
}

/// A NAT-PMP gateway that grants every request.
async fn fake_natpmp_gateway() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            match (n, buf[1]) {
                (2, 0) => {
                    let mut r = vec![0, 128, 0, 0, 0, 0, 0, 7];
                    r.extend_from_slice(&[203, 0, 113, 9]);
                    let _ = sock.send_to(&r, from).await;
                }
                (12, op @ (1 | 2)) => {
                    let mut r = vec![0, 128 + op, 0, 0, 0, 0, 0, 7];
                    r.extend_from_slice(&buf[4..6]);
                    r.extend_from_slice(&buf[6..8]);
                    r.extend_from_slice(&buf[8..12]);
                    let _ = sock.send_to(&r, from).await;
                }
                _ => {}
            }
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_natpmp_port_mapping() {
    setup_test_logging();
    let gw = fake_natpmp_gateway().await;
    let dir = tempfile::TempDir::with_prefix("rqbit_net_natpmp").unwrap();
    let s = Session::new_with_opts(
        dir.path().to_owned(),
        SessionOptions {
            dht: None,
            listen: Some(listener()),
            disable_local_service_discovery: true,
            port_mapping: Some(PortMappingConfig {
                natpmp: true,
                natpmp_gateway: Some(gw),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let port = s.listen_addr().unwrap().port();
    let has = |proto: MappingProtocol, p: u16| {
        s.port_mapping_status().mappings.iter().any(|m| {
            m.transport == MappingTransport::NatPmp
                && m.protocol == proto
                && m.internal_port == p
                && m.external_port == p
        })
    };
    wait_until(
        || {
            ensure!(
                has(MappingProtocol::Tcp, port),
                "{:?}",
                s.port_mapping_status()
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let st = s.port_mapping_status();
    assert_eq!(
        st.external_ip,
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)))
    );
    // A documentation address is no vote for our public address.
    assert_eq!(s.external_ips(), (None, None));
    assert!(st.requested.contains(&(MappingProtocol::Tcp, port)));

    // Extra ports (e.g. the WebUI).
    assert!(s.add_port_mapping(MappingProtocol::Tcp, 8080));
    assert!(!s.add_port_mapping(MappingProtocol::Tcp, 8080));
    wait_until(
        || {
            ensure!(
                has(MappingProtocol::Tcp, 8080) && has(MappingProtocol::Tcp, port),
                "not mapped"
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();

    // A new listen port is mapped instead of the old one.
    let new_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    s.set_listen_port(new_port).await.unwrap();
    wait_until(
        || {
            ensure!(
                has(MappingProtocol::Tcp, new_port) && !has(MappingProtocol::Tcp, port),
                "{:?}",
                s.port_mapping_status()
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();

    s.set_port_mapping(PortMappingConfig::default());
    assert!(s.port_mapping_status().mappings.is_empty());
    s.stop().await;
}

// ---------------------------------------------------------------- I2P

const TRACKER_DEST_CHAR: char = 'T';

fn fake_dest(c: char) -> String {
    std::iter::repeat_n(c, 516).collect::<String>() + "AAAA"
}

type Acceptors = Arc<Mutex<std::collections::HashMap<String, Vec<TcpStream>>>>;

/// A SAM v3.1 bridge for tests: every session gets its own destination;
/// STREAM CONNECT to a session destination is paired with one of that
/// session's pending STREAM ACCEPTs, STREAM CONNECT to the tracker
/// destination goes to a local HTTP server.
async fn fake_sam(tracker: SocketAddr) -> u16 {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let sessions: Arc<Mutex<std::collections::HashMap<String, String>>> = Default::default();
    let acceptors: Acceptors = Default::default();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            let sessions = sessions.clone();
            let acceptors = acceptors.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(s);
                let mut my_dest: Option<String> = None;
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let word = |key: &str| {
                        line.split_whitespace()
                            .find_map(|w| w.strip_prefix(&format!("{key}=")))
                            .map(str::to_owned)
                    };
                    let reply = if line.starts_with("HELLO") {
                        "HELLO REPLY RESULT=OK VERSION=3.1\n".to_owned()
                    } else if line.starts_with("SESSION CREATE") {
                        let id = word("ID").unwrap();
                        let mut g = sessions.lock();
                        let c = char::from(b'B' + u8::try_from(g.len()).unwrap());
                        let d = fake_dest(c);
                        g.insert(id, d.clone());
                        my_dest = Some(d);
                        "SESSION STATUS RESULT=OK DESTINATION=PRIV\n".to_owned()
                    } else if line.starts_with("NAMING LOOKUP NAME=ME") {
                        format!(
                            "NAMING REPLY RESULT=OK NAME=ME VALUE={}\n",
                            my_dest.clone().unwrap()
                        )
                    } else if line.starts_with("NAMING LOOKUP NAME=tracker.i2p") {
                        format!(
                            "NAMING REPLY RESULT=OK NAME=tracker.i2p VALUE={}\n",
                            fake_dest(TRACKER_DEST_CHAR)
                        )
                    } else if line.starts_with("NAMING LOOKUP") {
                        "NAMING REPLY RESULT=KEY_NOT_FOUND\n".to_owned()
                    } else if line.starts_with("STREAM ACCEPT") {
                        let d = sessions.lock().get(&word("ID").unwrap()).cloned().unwrap();
                        r.get_mut()
                            .write_all(b"STREAM STATUS RESULT=OK\n")
                            .await
                            .unwrap();
                        acceptors.lock().entry(d).or_default().push(r.into_inner());
                        return;
                    } else if line.starts_with("STREAM CONNECT") {
                        let from = sessions.lock().get(&word("ID").unwrap()).cloned().unwrap();
                        let to = word("DESTINATION").unwrap();
                        let mut conn = r.into_inner();
                        if to == fake_dest(TRACKER_DEST_CHAR) {
                            let mut up = TcpStream::connect(tracker).await.unwrap();
                            conn.write_all(b"STREAM STATUS RESULT=OK\n").await.unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut conn, &mut up).await;
                            return;
                        }
                        let mut peer = None;
                        for _ in 0..200 {
                            peer = acceptors.lock().get_mut(&to).and_then(|v| v.pop());
                            if peer.is_some() {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(25)).await;
                        }
                        let Some(mut peer) = peer else {
                            let _ = conn
                                .write_all(b"STREAM STATUS RESULT=CANT_REACH_PEER\n")
                                .await;
                            return;
                        };
                        peer.write_all(format!("{from} FROM_PORT=0 TO_PORT=0\n").as_bytes())
                            .await
                            .unwrap();
                        conn.write_all(b"STREAM STATUS RESULT=OK\n").await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut conn, &mut peer).await;
                        return;
                    } else {
                        "ERROR\n".to_owned()
                    };
                    r.get_mut().write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    port
}

/// An HTTP tracker for I2P: remembers `ip=<dest>.i2p` of announcers and
/// returns the others as dictionary peers.
async fn fake_i2p_tracker() -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let announces = Arc::new(AtomicUsize::new(0));
    let a2 = announces.clone();
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let seen = seen.clone();
            let a2 = a2.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut b).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).to_string();
                let path = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                assert!(path.starts_with("/announce?"), "{path}");
                let dest = path
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("ip="))
                    .map(|v| urlencoding::decode(v).unwrap().into_owned())
                    .and_then(|v| v.strip_suffix(".i2p").map(str::to_owned))
                    .unwrap();
                a2.fetch_add(1, Ordering::SeqCst);
                let mut body = b"d8:intervali60e5:peersl".to_vec();
                {
                    let mut g = seen.lock();
                    for d in g.iter().filter(|d| **d != dest) {
                        let ip = format!("{d}.i2p");
                        body.extend_from_slice(format!("d2:ip{}:{ip}e", ip.len()).as_bytes());
                    }
                    if !g.contains(&dest) {
                        g.push(dest);
                    }
                }
                body.extend_from_slice(b"ee");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.write_all(&body).await;
            });
        }
    });
    (addr, announces)
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_net_i2p_swarm_through_sam() {
    setup_test_logging();
    let (tracker, announces) = fake_i2p_tracker().await;
    let sam_port = fake_sam(tracker).await;
    let i2p = crate::I2pSettings {
        sam: crate::SamOptions {
            port: sam_port,
            ..Default::default()
        },
        mixed_mode: false,
    };

    let dir = create_default_random_dir_with_torrents(2, 200_000, Some("rqbit_i2p"));
    let torrent = create_torrent(
        dir.path(),
        crate::CreateTorrentOptions {
            piece_length: Some(32768),
            trackers: vec!["http://tracker.i2p/announce".to_owned()],
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap()
    .as_bytes()
    .unwrap();

    let seeder = session(dir.path(), true, NetSettings::default())
        .await
        .unwrap();
    let ldir = tempfile::TempDir::with_prefix("rqbit_i2p_leech").unwrap();
    let leecher = session(ldir.path(), false, NetSettings::default())
        .await
        .unwrap();
    for s in [&seeder, &leecher] {
        s.set_i2p(Some(i2p.clone()));
    }
    wait_until(
        || {
            ensure!(
                seeder.i2p_status().connected && leecher.i2p_status().connected,
                "SAM session not up"
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let seeder_dest = seeder.i2p_status().destination.unwrap();
    assert!(seeder.i2p_status().b32.unwrap().ends_with(".b32.i2p"));

    let _seed = add_seed(&seeder, torrent.clone(), dir.path())
        .await
        .unwrap();
    wait_until(
        || {
            ensure!(
                announces.load(Ordering::SeqCst) >= 1,
                "seeder didn't announce"
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();

    let blocked: Arc<Mutex<Vec<BlockedPeer>>> = Default::default();
    let b2 = blocked.clone();
    leecher.set_blocked_peer_callback(Some(Arc::new(move |b| b2.lock().push(b))));
    // The clearnet address of the seeder is refused: mixed mode is off.
    let leech = add_leech(&leecher, torrent, vec![seeder.listen_addr().unwrap()])
        .await
        .unwrap();
    wait_finished(&leech, 60).await.unwrap();
    assert!(
        blocked
            .lock()
            .iter()
            .any(|b| b.reason == PeerBlockReason::I2pMixedMode && !b.incoming)
    );
    let i2p_addr = leecher.i2p_peer_addr(&seeder_dest).unwrap();
    assert_eq!(
        leecher.i2p_destination_of(i2p_addr).as_deref(),
        Some(seeder_dest.as_str())
    );
    assert!(
        leech
            .trackers()
            .iter()
            .any(|t| t.url.contains("tracker.i2p") && t.state == crate::TrackerState::Working)
    );

    // Mixed mode off: a clearnet tracker of the I2P torrent is never
    // contacted (it would learn our address).
    let clearnet = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let clearnet_url = format!("http://{}/announce", clearnet.local_addr().unwrap());
    let contacted = Arc::new(AtomicUsize::new(0));
    let c2 = contacted.clone();
    tokio::spawn(async move {
        while let Ok((_s, _)) = clearnet.accept().await {
            c2.fetch_add(1, Ordering::SeqCst);
        }
    });
    leech.add_trackers(vec![(clearnet_url.clone(), 5)]);
    leech.force_reannounce(Some(&clearnet_url));
    wait_until(
        || {
            ensure!(
                leech
                    .trackers()
                    .iter()
                    .any(|t| t.url == clearnet_url && t.message.contains("mixed mode")),
                "clearnet tracker not skipped: {:?}",
                leech.trackers()
            );
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(contacted.load(Ordering::SeqCst), 0);

    leecher.set_i2p(None);
    assert!(!leecher.i2p_status().enabled);
    leecher.stop().await;
    seeder.stop().await;
}
