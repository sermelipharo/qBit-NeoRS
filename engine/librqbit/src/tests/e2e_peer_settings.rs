// End-to-end checks of the session peer settings that mirror libtorrent's
// `seeding_outgoing_connections`, `max_allowed_in_request_queue`,
// `allow_multiple_connections_per_pid` and
// `allow_multiple_connections_per_ip`, and of the automatic ban of peers
// sending corrupt data, over local TCP connections.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use librqbit_core::hash_id::Id20;

use crate::{
    AddTorrent, AddTorrentOptions, BlockedPeer, ListenerMode, ManagedTorrent, PeerBlockReason,
    PeerPolicyOptions, Session, SessionOptions, create_torrent,
    limits::LimitsConfig,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    tests::test_util::{
        DropChecks, create_default_random_dir_with_torrents, setup_test_logging, wait_until,
        wait_until_i_am_the_last_task,
    },
};

async fn make_torrent(num_files: usize, file_size: usize) -> (tempfile::TempDir, Bytes) {
    let dir = create_default_random_dir_with_torrents(num_files, file_size, Some("rqbit_peerset"));
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

fn local_listener() -> Option<ListenerOptions> {
    Some(ListenerOptions {
        mode: ListenerMode::TcpOnly,
        listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
        ..Default::default()
    })
}

async fn new_session(
    dir: &Path,
    listen: bool,
    peer_id: Option<Id20>,
    peer_policy: PeerPolicyOptions,
) -> anyhow::Result<Arc<Session>> {
    Session::new_with_opts(
        dir.to_owned(),
        SessionOptions {
            dht: None,
            listen: if listen { local_listener() } else { None },
            disable_local_service_discovery: true,
            peer_id,
            peer_policy,
            ..Default::default()
        },
    )
    .await
}

async fn start_seeder(
    torrent: Bytes,
    dir: &Path,
    peer_id: Option<Id20>,
    peer_policy: PeerPolicyOptions,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>, SocketAddr)> {
    let session = new_session(
        &std::env::temp_dir().join("does_not_exist"),
        true,
        peer_id,
        peer_policy,
    )
    .await?;
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
    let addr = session.listen_addr().context("no listen addr")?;
    Ok((session, handle, addr))
}

// A live (unpaused) leecher; `listen` makes it reachable.
async fn start_leecher(
    torrent: Bytes,
    out: &Path,
    listen: bool,
    peer_id: Option<Id20>,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>)> {
    start_leecher_with(torrent, out, listen, peer_id, Default::default()).await
}

async fn start_leecher_with(
    torrent: Bytes,
    out: &Path,
    listen: bool,
    peer_id: Option<Id20>,
    peer_policy: PeerPolicyOptions,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>)> {
    let session = new_session(out, listen, peer_id, peer_policy).await?;
    let handle = session
        .add_torrent(
            AddTorrent::TorrentFileBytes(torrent),
            Some(AddTorrentOptions {
                paused: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    wait_until(
        || {
            ensure!(!handle.piece_states().is_empty(), "not checked yet");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await?;
    session.unpause(&handle).await?;
    Ok((session, handle))
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

async fn run(name: &str, f: impl AsyncFnOnce(&DropChecks) -> anyhow::Result<()>) {
    setup_test_logging();
    let drop_checks = DropChecks::default();
    tokio::time::timeout(Duration::from_secs(120), f(&drop_checks))
        .await
        .context(format!("{name} timed out"))
        .unwrap()
        .unwrap();
    wait_until_i_am_the_last_task().await.unwrap();
    drop_checks.check().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_seeding_outgoing_connections() {
    run("seeding_outgoing_connections", seeding_outgoing_connections).await;
}

// A finished torrent doesn't dial peers while the setting is off; turning it
// on dials the peers skipped meanwhile.
async fn seeding_outgoing_connections(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(2, 200_000).await;
    let (server, server_handle, _) = start_seeder(
        torrent.clone(),
        dir.path(),
        None,
        PeerPolicyOptions {
            seeding_outgoing_connections: false,
            ..Default::default()
        },
    )
    .await?;
    drop_checks.add(&server, "server session");

    let out = tempfile::TempDir::with_prefix("rqbit_peerset_client").unwrap();
    let (client, handle) = start_leecher(torrent, out.path(), true, None).await?;
    drop_checks.add(&client, "client session");
    let client_addr = client.listen_addr().context("no client listen addr")?;

    // The seeder learns about the leecher, but doesn't connect.
    assert_eq!(server_handle.add_peers(vec![client_addr]), 1);
    wait_until(
        || {
            let s = server_handle.live().context("not live")?.stats_snapshot();
            ensure!(s.peer_stats.not_needed == 1, "peer not skipped yet");
            Ok(())
        },
        Duration::from_secs(10),
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(server_handle.connections(), 0);
    assert_eq!(handle.connections(), 0);
    assert!(!handle.stats().finished);

    // Turned on: the skipped peer is dialled and gets the torrent.
    server.set_seeding_outgoing_connections(true);
    wait_finished(&handle, 60).await?;

    drop(handle);
    drop(server_handle);
    drop(client);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_max_allowed_in_request_queue() {
    run("max_allowed_in_request_queue", max_allowed_in_request_queue).await;
}

// A seeder that allows a single queued request (advertised as reqq=1) still
// serves the whole torrent: the leecher keeps within it.
async fn max_allowed_in_request_queue(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(2, 200_000).await;
    let (server, server_handle, server_addr) = start_seeder(
        torrent.clone(),
        dir.path(),
        None,
        PeerPolicyOptions {
            max_allowed_in_request_queue: 1,
            ..Default::default()
        },
    )
    .await?;
    drop_checks.add(&server, "server session");
    assert_eq!(server.peer_policy().max_allowed_in_request_queue, 1);

    let out = tempfile::TempDir::with_prefix("rqbit_peerset_reqq").unwrap();
    let (client, handle) = start_leecher(torrent, out.path(), false, None).await?;
    drop_checks.add(&client, "client session");
    handle.add_peers(vec![server_addr]);
    wait_finished(&handle, 60).await?;

    // Changed at runtime (and clamped).
    server.set_max_allowed_in_request_queue(0);
    assert_eq!(server.peer_policy().max_allowed_in_request_queue, 1);
    server.set_max_allowed_in_request_queue(500);
    assert_eq!(server.peer_policy().max_allowed_in_request_queue, 500);

    drop(handle);
    drop(server_handle);
    drop(client);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_duplicate_peer_id() {
    run("duplicate_peer_id", duplicate_peer_id).await;
}

// Two leechers with the same peer id: the seeder keeps one connection while
// `allow_multiple_connections_per_pid` is off, and takes another one once it
// is on.
async fn duplicate_peer_id(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(4, 500_000).await;
    // The leechers' id is lower than the seeder's: the seeder closes the new
    // incoming connection (libtorrent's rule).
    let seeder_id = Id20::new([0xee; 20]);
    let leecher_id = Some(Id20::new(*b"-XX0001-111111111111"));
    let (server, server_handle, server_addr) = start_seeder(
        torrent.clone(),
        dir.path(),
        Some(seeder_id),
        Default::default(),
    )
    .await?;
    drop_checks.add(&server, "server session");
    // Keep the first leecher downloading for the whole test.
    server_handle.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(60_000),
        download_bps: None,
    });

    let mut clients = Vec::new();
    let start = async |i: usize| {
        let out = tempfile::TempDir::with_prefix("rqbit_peerset_pid").unwrap();
        let (client, handle) =
            start_leecher(torrent.clone(), out.path(), false, leecher_id).await?;
        drop_checks.add(&client, format!("client session {i}"));
        handle.add_peers(vec![server_addr]);
        anyhow::Ok((client, handle, out))
    };

    clients.push(start(0).await?);
    wait_until(
        || {
            ensure!(
                server_handle.connections() == 1,
                "first leecher not connected"
            );
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    clients.push(start(1).await?);
    // The second connection is refused and the first one stays.
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(server_handle.connections(), 1);
    }
    assert_eq!(clients[0].1.connections(), 1);
    assert_eq!(clients[1].1.connections(), 0);

    server.set_allow_multiple_connections_per_pid(true);
    clients.push(start(2).await?);
    wait_until(
        || {
            ensure!(
                server_handle.connections() == 2,
                "third leecher not connected"
            );
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    assert_eq!(clients[0].1.connections(), 1);

    drop(server_handle);
    drop(clients);
    drop(server);
    Ok(())
}

fn one_connection_per_ip() -> PeerPolicyOptions {
    PeerPolicyOptions {
        allow_multiple_connections_per_ip: false,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_one_incoming_connection_per_ip() {
    run("one_incoming_connection_per_ip", one_incoming_connection_per_ip).await;
}

// Leechers on 127.0.0.1 with different peer ids: the seeder keeps one
// incoming connection while `allow_multiple_connections_per_ip` is off, and
// takes another one once it is on.
async fn one_incoming_connection_per_ip(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(4, 500_000).await;
    let (server, server_handle, server_addr) =
        start_seeder(torrent.clone(), dir.path(), None, one_connection_per_ip()).await?;
    drop_checks.add(&server, "server session");
    // Keep the first leecher downloading for the whole test.
    server_handle.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(60_000),
        download_bps: None,
    });

    let mut clients = Vec::new();
    let start = async |i: usize| {
        let out = tempfile::TempDir::with_prefix("rqbit_peerset_ip").unwrap();
        let (client, handle) = start_leecher(torrent.clone(), out.path(), false, None).await?;
        drop_checks.add(&client, format!("client session {i}"));
        handle.add_peers(vec![server_addr]);
        anyhow::Ok((client, handle, out))
    };

    clients.push(start(0).await?);
    wait_until(
        || {
            ensure!(
                server_handle.connections() == 1,
                "first leecher not connected"
            );
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    clients.push(start(1).await?);
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(server_handle.connections(), 1);
    }
    assert_eq!(clients[0].1.connections(), 1);
    assert_eq!(clients[1].1.connections(), 0);

    server.set_allow_multiple_connections_per_ip(true);
    assert!(server.peer_policy().allow_multiple_connections_per_ip);
    clients.push(start(2).await?);
    wait_until(
        || {
            ensure!(
                server_handle.connections() == 2,
                "third leecher not connected"
            );
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    assert_eq!(clients[0].1.connections(), 1);

    drop(server_handle);
    drop(clients);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_one_outgoing_connection_per_ip() {
    run("one_outgoing_connection_per_ip", one_outgoing_connection_per_ip).await;
}

// Two seeders on 127.0.0.1: a leecher with `allow_multiple_connections_per_ip`
// off dials only one of them; turning it on dials the other.
async fn one_outgoing_connection_per_ip(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(4, 500_000).await;
    let mut seeders = Vec::new();
    for i in 0..2 {
        let (server, handle, addr) =
            start_seeder(torrent.clone(), dir.path(), None, Default::default()).await?;
        drop_checks.add(&server, format!("server session {i}"));
        // Slow, so the leecher stays connected for the whole test.
        handle.set_ratelimits(LimitsConfig {
            upload_bps: std::num::NonZeroU32::new(30_000),
            download_bps: None,
        });
        seeders.push((server, handle, addr));
    }
    let out = tempfile::TempDir::with_prefix("rqbit_peerset_ip_out").unwrap();
    let (client, handle) =
        start_leecher_with(torrent, out.path(), false, None, one_connection_per_ip()).await?;
    drop_checks.add(&client, "client session");
    handle.add_peers(seeders.iter().map(|s| s.2).collect());
    wait_until(
        || {
            ensure!(handle.connections() == 1, "leecher not connected");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(handle.connections(), 1);
    }
    assert_eq!(
        seeders.iter().map(|s| s.1.connections()).sum::<u32>(),
        1,
        "only one seeder is connected"
    );

    client.set_allow_multiple_connections_per_ip(true);
    wait_until(
        || {
            ensure!(handle.connections() == 2, "second seeder not dialled");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;

    drop(handle);
    drop(client);
    drop(seeders);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_corrupt_peer_is_banned() {
    run("corrupt_peer_is_banned", corrupt_peer_is_banned).await;
}

// Overwrites every file under `dir` with garbage of the same size, in place
// (the seeder keeps serving them without checking).
fn corrupt_files(dir: &Path) {
    for entry in walkdir::WalkDir::new(dir) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let len = usize::try_from(entry.metadata().unwrap().len()).unwrap();
            std::fs::write(entry.path(), vec![0xa5u8; len]).unwrap();
        }
    }
}

// A seeder serving corrupt data is banned at its first failed piece (it sent
// the whole piece): reported once through the blocked-peer callback, not
// dialled again (even after a pause), its incoming connections refused,
// while a good seeder completes the download.
async fn corrupt_peer_is_banned(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(2, 300_000).await;
    let bad_dir = tempfile::TempDir::with_prefix("rqbit_peerset_bad").unwrap();
    for entry in walkdir::WalkDir::new(dir.path()).min_depth(1) {
        let entry = entry.unwrap();
        let to = bad_dir
            .path()
            .join(entry.path().strip_prefix(dir.path()).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&to).unwrap();
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }

    let (good, good_handle, good_addr) =
        start_seeder(torrent.clone(), dir.path(), None, Default::default()).await?;
    drop_checks.add(&good, "good seeder session");
    let (bad, bad_handle, bad_addr) =
        start_seeder(torrent.clone(), bad_dir.path(), None, Default::default()).await?;
    drop_checks.add(&bad, "bad seeder session");
    // Checked and seeding: now it serves garbage.
    corrupt_files(bad_dir.path());

    let out = tempfile::TempDir::with_prefix("rqbit_peerset_ban").unwrap();
    let (client, handle) = start_leecher(torrent, out.path(), true, None).await?;
    drop_checks.add(&client, "client session");
    let client_addr = client.listen_addr().context("no client listen addr")?;
    let blocked: Arc<parking_lot::Mutex<Vec<BlockedPeer>>> = Default::default();
    let b2 = blocked.clone();
    client.set_blocked_peer_callback(Some(Arc::new(move |b| b2.lock().push(b))));

    // Only the bad seeder at first: its first piece fails the hash check.
    assert_eq!(handle.add_peers(vec![bad_addr]), 1);
    wait_until(
        || {
            ensure!(!blocked.lock().is_empty(), "not banned yet");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await?;
    assert_eq!(
        *blocked.lock(),
        vec![BlockedPeer {
            addr: bad_addr,
            reason: PeerBlockReason::CorruptData,
            incoming: false,
        }]
    );
    assert!(handle.wasted_bytes() > 0, "the failed piece is wasted");

    // Not dialled again, not even when added again after a pause.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(handle.connections(), 0);
    assert_eq!(bad_handle.connections(), 0);
    client.pause(&handle).await?;
    client.unpause(&handle).await?;
    wait_until(
        || {
            ensure!(handle.live().is_some(), "not live again");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await?;
    assert_eq!(handle.add_peers(vec![bad_addr]), 0, "banned peer was added");

    // Its connections to us are refused.
    assert_eq!(bad_handle.add_peers(vec![client_addr]), 1);
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(bad_handle.connections(), 0);
        assert_eq!(handle.connections(), 0);
    }

    // The good seeder completes the download; the ban was reported once.
    assert_eq!(handle.add_peers(vec![good_addr]), 1);
    wait_finished(&handle, 60).await?;
    assert_eq!(blocked.lock().len(), 1);
    assert_eq!(bad_handle.connections(), 0);

    drop(handle);
    drop(good_handle);
    drop(bad_handle);
    drop(client);
    drop(good);
    drop(bad);
    Ok(())
}
