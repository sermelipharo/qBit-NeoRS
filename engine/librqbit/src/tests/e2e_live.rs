// End-to-end checks of the live torrent controls: file priorities, peer
// details, piece export, rate limits, transfer stats and super seeding, over
// real local TCP connections.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail, ensure};
use bytes::Bytes;
use tracing::info;

use crate::{
    AddTorrent, AddTorrentOptions, ConnectionKind, ListenerMode, ManagedTorrent, PIECE_STATE_HAVE,
    PIECE_STATE_MISSING, PeerPolicyOptions, PeerSource, Session, SessionOptions, create_torrent,
    limits::LimitsConfig,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    tests::test_util::{
        DropChecks, create_default_random_dir_with_torrents, setup_test_logging, wait_until,
        wait_until_i_am_the_last_task,
    },
};

async fn make_torrent(
    num_files: usize,
    file_size: usize,
    piece_length: u32,
) -> (tempfile::TempDir, Bytes) {
    let dir = create_default_random_dir_with_torrents(num_files, file_size, Some("rqbit_live"));
    let torrent = create_torrent(
        dir.path(),
        crate::CreateTorrentOptions {
            piece_length: Some(piece_length),
            ..Default::default()
        },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let bytes = torrent.as_bytes().unwrap();
    (dir, bytes)
}

async fn start_seeder(
    torrent: Bytes,
    dir: &Path,
    peer_policy: PeerPolicyOptions,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>, SocketAddr)> {
    let session = Session::new_with_opts(
        std::env::temp_dir().join("does_not_exist"),
        SessionOptions {
            dht: None,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            peer_policy,
            ..Default::default()
        },
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

async fn start_leecher(
    torrent: Bytes,
    out: &Path,
) -> anyhow::Result<(Arc<Session>, Arc<ManagedTorrent>)> {
    let session = Session::new_with_opts(
        out.to_owned(),
        SessionOptions {
            dht: None,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await?;
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
    Ok((session, handle))
}

// The initial check of a torrent added paused must be over before unpausing:
// an unpause during the check is not remembered.
async fn wait_checked(handle: &ManagedTorrent) -> anyhow::Result<()> {
    wait_until(
        || {
            ensure!(!handle.piece_states().is_empty(), "not checked yet");
            Ok(())
        },
        Duration::from_secs(30),
    )
    .await
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

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_file_priorities_and_live_info() {
    setup_test_logging();
    let drop_checks = DropChecks::default();
    tokio::time::timeout(
        Duration::from_secs(120),
        file_priorities_and_live_info(&drop_checks),
    )
    .await
    .context("timed out")
    .unwrap()
    .unwrap();
    wait_until_i_am_the_last_task().await.unwrap();
    drop_checks.check().unwrap();
}

async fn file_priorities_and_live_info(drop_checks: &DropChecks) -> anyhow::Result<()> {
    // 4 files of 300 KB in 32 KiB pieces: pieces cross file boundaries.
    let (dir, torrent) = make_torrent(4, 300_000, 32768).await;
    let (server, server_handle, server_addr) = start_seeder(
        torrent.clone(),
        dir.path(),
        PeerPolicyOptions {
            max_uploads: Some(4),
            max_uploads_per_torrent: Some(2),
            ..Default::default()
        },
    )
    .await?;
    drop_checks.add(&server, "server session");
    // Slow the upload down so the transfer can be observed (below one block
    // per burst would also work, see limits.rs).
    server_handle.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(200_000),
        download_bps: None,
    });
    assert_eq!(server_handle.max_uploads(), Some(2));

    let out = tempfile::TempDir::with_prefix("rqbit_live_client").unwrap();
    let (client, handle) = start_leecher(torrent.clone(), out.path()).await?;
    drop_checks.add(&client, "client session");

    // Set while the torrent is still checking / paused, and kept across unpause.
    let priorities = vec![1, 0, 7, 0];
    handle.set_file_priorities(priorities.clone())?;
    handle.set_sequential(true);
    assert!(handle.set_file_priorities(vec![1]).is_err());
    // Queued while not live, connected on start.
    assert_eq!(handle.add_peers(vec![server_addr]), 1);
    wait_checked(&handle).await?;
    assert_eq!(handle.file_priorities(), priorities);
    assert_eq!(handle.pieces_have(), 0);
    client.unpause(&handle).await?;

    // Observe the connection while downloading.
    wait_until(
        || {
            let peers = handle.peers_detailed();
            let p = peers.first().context("no peers")?;
            ensure!(
                p.flags.am_interested && !p.flags.peer_choking,
                "{:?}",
                p.flags
            );
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    let peers = handle.peers_detailed();
    let p = &peers[0];
    info!(?p, "client side peer");
    assert_eq!(p.addr, server_addr);
    assert!(matches!(p.connection, ConnectionKind::Tcp));
    assert_eq!(p.client, crate::client_name_and_version());
    assert!(p.sources.contains(PeerSource::Manual));
    assert!(!p.flags.incoming && p.flags.seed);
    assert_eq!(p.progress, 1.0);
    assert!(p.relevance > 0.0);
    let total_pieces = handle.piece_states().len();
    assert_eq!(handle.piece_availability(), vec![1; total_pieces]);
    let dc = handle.distributed_copies();
    assert!((1.0..2.0).contains(&dc), "{dc}");

    let server_peers = server_handle.peers_detailed();
    let sp = server_peers.first().context("server has no peers")?;
    info!(?sp, "server side peer");
    assert!(sp.flags.incoming && sp.flags.peer_interested && !sp.flags.am_choking);
    assert!(sp.sources.contains(PeerSource::Incoming));
    assert_eq!(sp.client, crate::client_name_and_version());

    wait_finished(&handle, 60).await?;

    // Pieces belonging only to priority-0 files were not downloaded.
    let file_infos = handle.with_metadata(|m| m.file_infos.clone())?;
    let states = handle.piece_states();
    let mut skipped = 0;
    for (piece, state) in states.iter().enumerate() {
        let wanted = file_infos
            .iter()
            .zip(&priorities)
            .any(|(fi, prio)| *prio > 0 && fi.piece_range_usize().contains(&piece));
        if wanted {
            assert_eq!(*state, PIECE_STATE_HAVE, "piece {piece}");
        } else {
            assert_eq!(*state, PIECE_STATE_MISSING, "piece {piece}");
            skipped += 1;
        }
    }
    assert!(skipped > 0);
    let file_progress = handle.stats().file_progress;
    assert_eq!(file_progress[0], 300_000);
    assert_eq!(file_progress[2], 300_000);
    assert!(file_progress[1] < 300_000 && file_progress[3] < 300_000);

    let cs = client.transfer_stats();
    info!(?cs, "client stats");
    assert!(cs.payload_downloaded >= 600_000);
    assert!(cs.protocol_downloaded > cs.payload_downloaded);
    assert!(cs.ip_overhead_downloaded > 0 && cs.protocol_uploaded > 0);
    let ss = server.transfer_stats();
    info!(?ss, "server stats");
    assert!(ss.payload_uploaded >= 600_000);
    assert!(ss.protocol_uploaded > ss.payload_uploaded);

    // Wanting everything resumes the download from the same peer.
    handle.set_file_priorities(vec![1; 4])?;
    wait_finished(&handle, 60).await?;
    assert!(handle.piece_states().iter().all(|s| *s == PIECE_STATE_HAVE));
    assert_eq!(handle.distributed_copies(), -1.0);
    assert!(handle.piece_availability().is_empty());
    assert_eq!(handle.wasted_bytes(), 0);

    drop(handle);
    drop(server_handle);
    drop(client);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_super_seeding() {
    setup_test_logging();
    let drop_checks = DropChecks::default();
    tokio::time::timeout(Duration::from_secs(120), super_seeding(&drop_checks))
        .await
        .context("timed out")
        .unwrap()
        .unwrap();
    wait_until_i_am_the_last_task().await.unwrap();
    drop_checks.check().unwrap();
}

async fn super_seeding(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(2, 200_000, 16384).await;
    let (server, server_handle, server_addr) =
        start_seeder(torrent.clone(), dir.path(), Default::default()).await?;
    drop_checks.add(&server, "server session");
    server_handle.set_super_seeding(true);
    assert!(server_handle.super_seeding());
    // The whole torrent moves over loopback in a few milliseconds, which is
    // shorter than the poll interval below: the leecher could finish and
    // disconnect before the flag is ever observed. Slow the upload down so the
    // connection lives for a few seconds.
    server_handle.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(100_000),
        download_bps: None,
    });

    let out = tempfile::TempDir::with_prefix("rqbit_live_ss_client").unwrap();
    let (client, handle) = start_leecher(torrent, out.path()).await?;
    drop_checks.add(&client, "client session");
    handle.add_peers(vec![server_addr]);
    wait_checked(&handle).await?;
    client.unpause(&handle).await?;

    wait_until(
        || {
            let peers = server_handle.peers_detailed();
            let p = peers.first().context("no peers")?;
            ensure!(p.flags.super_seeding, "not super seeding");
            Ok(())
        },
        Duration::from_secs(20),
    )
    .await?;
    wait_finished(&handle, 90).await?;
    assert!(handle.piece_states().iter().all(|s| *s == PIECE_STATE_HAVE));

    drop(handle);
    drop(server_handle);
    drop(client);
    drop(server);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_upload_slots() {
    setup_test_logging();
    let drop_checks = DropChecks::default();
    tokio::time::timeout(Duration::from_secs(120), upload_slots(&drop_checks))
        .await
        .context("timed out")
        .unwrap()
        .unwrap();
    wait_until_i_am_the_last_task().await.unwrap();
    drop_checks.check().unwrap();
}

// A seeder with a single upload slot serves two leechers one after the other
// and never unchokes both at once.
async fn upload_slots(drop_checks: &DropChecks) -> anyhow::Result<()> {
    let (dir, torrent) = make_torrent(2, 300_000, 32768).await;
    let (server, server_handle, server_addr) = start_seeder(
        torrent.clone(),
        dir.path(),
        PeerPolicyOptions {
            max_uploads: Some(1),
            ..Default::default()
        },
    )
    .await?;
    drop_checks.add(&server, "server session");
    server_handle.set_ratelimits(LimitsConfig {
        upload_bps: std::num::NonZeroU32::new(400_000),
        download_bps: None,
    });

    let mut clients = Vec::new();
    for i in 0..2 {
        let out = tempfile::TempDir::with_prefix("rqbit_live_slots").unwrap();
        let (client, handle) = start_leecher(torrent.clone(), out.path()).await?;
        drop_checks.add(&client, format!("client session {i}"));
        handle.add_peers(vec![server_addr]);
        wait_checked(&handle).await?;
        client.unpause(&handle).await?;
        clients.push((client, handle, out));
    }

    let mut max_unchoked = 0;
    let mut saw_two_peers = false;
    wait_until(
        || {
            let peers = server_handle.peers_detailed();
            saw_two_peers |= peers.len() == 2;
            let unchoked = peers.iter().filter(|p| !p.flags.am_choking).count();
            max_unchoked = max_unchoked.max(unchoked);
            for (_, h, _) in &clients {
                ensure!(h.stats().finished, "not finished");
            }
            Ok(())
        },
        Duration::from_secs(90),
    )
    .await?;
    assert!(saw_two_peers);
    assert_eq!(max_unchoked, 1);
    let stats = server.transfer_stats();
    assert!(stats.payload_uploaded >= 1_200_000, "{stats:?}");

    drop(server_handle);
    drop(clients);
    drop(server);
    Ok(())
}
