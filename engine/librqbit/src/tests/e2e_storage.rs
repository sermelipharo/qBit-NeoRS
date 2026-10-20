//! End-to-end tests of the storage operations, with local sessions only.

use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom, Write},
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Context;
use bytes::Bytes;
use tempfile::TempDir;
use tokio::time::timeout;
use tracing::info;

use crate::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, DeleteOptions, ManagedTorrentState,
    Session, SessionOptions, SessionPersistenceConfig, create_torrent,
    listen::ListenerOptions,
    spawn_utils::BlockingSpawner,
    storage::{MoveMode, StorageSettings, TorrentErrorKind},
    tests::test_util::{
        TestPeerMetadata, create_default_random_dir_with_torrents, setup_test_logging,
    },
    torrent_state::ManagedTorrentHandle,
};

const PIECE_LENGTH: u32 = 16384;
const FILE_SIZE: usize = 50_000;
const TEST_TIMEOUT: Duration = Duration::from_secs(90);

struct TestTorrent {
    dir: TempDir,
    bytes: Bytes,
    // File names in torrent order.
    names: Vec<String>,
}

impl TestTorrent {
    async fn new(prefix: &str, num_files: usize) -> Self {
        let dir = create_default_random_dir_with_torrents(num_files, FILE_SIZE, Some(prefix));
        let torrent = create_torrent(
            dir.path(),
            CreateTorrentOptions {
                piece_length: Some(PIECE_LENGTH),
                ..Default::default()
            },
            &BlockingSpawner::new(1),
        )
        .await
        .unwrap();
        let bytes = torrent.as_bytes().unwrap();
        let names = librqbit_core::torrent_metainfo::torrent_from_bytes(&bytes)
            .unwrap()
            .info
            .data
            .validate()
            .unwrap()
            .iter_file_details()
            .map(|fd| fd.filename.to_string())
            .collect();
        Self { dir, bytes, names }
    }

    fn name(&self, idx: usize) -> &str {
        &self.names[idx]
    }

    fn original(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.dir.path().join(name)).unwrap()
    }
}

fn base_opts() -> SessionOptions {
    SessionOptions {
        dht: None,
        persistence: None,
        disable_local_service_discovery: true,
        peer_id: Some(TestPeerMetadata::good().as_peer_id()),
        ..Default::default()
    }
}

async fn listening_session(root: &Path) -> Arc<Session> {
    Session::new_with_opts(
        root.to_owned(),
        SessionOptions {
            listen: Some(ListenerOptions {
                listen_addr: (Ipv4Addr::LOCALHOST, 0).into(),
                ..Default::default()
            }),
            ..base_opts()
        },
    )
    .await
    .unwrap()
}

async fn client_session(
    root: &Path,
    storage: StorageSettings,
    persistence: Option<PathBuf>,
) -> Arc<Session> {
    Session::new_with_opts(
        root.to_owned(),
        SessionOptions {
            fastresume: persistence.is_some(),
            persistence: persistence.map(|folder| SessionPersistenceConfig::Json {
                folder: Some(folder),
            }),
            storage,
            ..base_opts()
        },
    )
    .await
    .unwrap()
}

/// A seeding session serving `t` from its own folder.
async fn seeder(t: &TestTorrent) -> (Arc<Session>, ManagedTorrentHandle, SocketAddr) {
    let session = listening_session(t.dir.path()).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(t.dir.path().to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;
    let addr = session.listen_addr().unwrap();
    (session, h, addr)
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> anyhow::Result<bool>) {
    timeout(TEST_TIMEOUT, async {
        loop {
            if cond().unwrap() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("timeout waiting for {what}"))
    .unwrap()
}

async fn wait_finished(h: &ManagedTorrentHandle) {
    wait_for("torrent to finish", || {
        h.with_state(|s| match s {
            ManagedTorrentState::Live(l) => Ok(l.is_finished()),
            ManagedTorrentState::Error(e) => anyhow::bail!("torrent errored: {e:#}"),
            _ => Ok(false),
        })
    })
    .await
}

async fn wait_paused(h: &ManagedTorrentHandle) {
    wait_for("torrent to be paused", || {
        h.with_state(|s| match s {
            ManagedTorrentState::Paused(_) => Ok(true),
            ManagedTorrentState::Error(e) => anyhow::bail!("torrent errored: {e:#}"),
            _ => Ok(false),
        })
    })
    .await
}

fn flip_byte(path: &Path, offset: u64) {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let mut b = [0u8; 1];
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.read_exact(&mut b).unwrap();
    b[0] ^= 0xff;
    f.seek(SeekFrom::Start(offset)).unwrap();
    f.write_all(&b).unwrap();
}

async fn download_and_compare(t: &TestTorrent, peer: SocketAddr) {
    let root = TempDir::with_prefix("storage_e2e_verify").unwrap();
    let session = client_session(root.path(), Default::default(), None).await;
    let out = root.path().join("out");
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![peer]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;
    for name in &t.names {
        assert_eq!(
            std::fs::read(out.join(name)).unwrap(),
            t.original(name),
            "{name} differs"
        );
    }
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_move_storage_live_and_paused_then_rename_and_delete() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_move", 3).await;
    let names = t.names.clone();
    // The seeder serves from a copy that gets moved around.
    let tmp = TempDir::with_prefix("storage_e2e_move_dst").unwrap();
    let start = tmp.path().join("start");
    std::fs::create_dir_all(&start).unwrap();
    for n in &names {
        std::fs::copy(t.dir.path().join(n), start.join(n)).unwrap();
    }
    let session = listening_session(tmp.path()).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(start.to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;
    let addr = session.listen_addr().unwrap();

    // Live move.
    let dst1 = tmp.path().join("dst1");
    timeout(
        TEST_TIMEOUT,
        h.move_storage(dst1.clone(), MoveMode::DontReplace),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(h.output_folder(), dst1);
    assert!(h.move_progress().is_none());
    assert!(!start.join(&names[0]).exists());
    assert_eq!(
        std::fs::read(dst1.join(&names[2])).unwrap(),
        t.original(&names[2])
    );
    wait_finished(&h).await;

    // Paused move: stays paused.
    session.pause(&h).await.unwrap();
    let dst2 = tmp.path().join("dst2");
    std::fs::create_dir_all(&dst2).unwrap();
    std::fs::write(dst2.join("unrelated.txt"), b"not ours").unwrap();
    h.move_storage(dst2.clone(), MoveMode::AlwaysReplace)
        .await
        .unwrap();
    assert!(h.is_paused());
    wait_paused(&h).await;
    assert!(!dst1.join(&names[0]).exists());
    assert_eq!(
        std::fs::read(dst2.join(&names[0])).unwrap(),
        t.original(&names[0])
    );
    session.unpause(&h).await.unwrap();
    wait_finished(&h).await;

    // Rename while live: on disk right away, and served from the new path.
    let idx = 1;
    h.rename_file(idx, "sub/one.bin").await.unwrap();
    assert!(dst2.join("sub/one.bin").exists());
    assert!(!dst2.join(&names[1]).exists());
    assert_eq!(h.file_paths()[idx], PathBuf::from("sub/one.bin"));
    assert!(h.rename_file(idx, "../escape").await.is_err());
    download_and_compare(&t, addr).await;

    // Delete with files through a trash callback: only our files go.
    let trashed = Arc::new(AtomicUsize::new(0));
    let counter = trashed.clone();
    session
        .delete_with_options(
            h.id().into(),
            DeleteOptions {
                delete_files: true,
                trash: Some(Box::new(move |p: &Path| {
                    counter.fetch_add(1, Ordering::Relaxed);
                    std::fs::remove_file(p)
                })),
            },
        )
        .await
        .unwrap();
    assert_eq!(trashed.load(Ordering::Relaxed), 3);
    let left: HashSet<_> = std::fs::read_dir(&dst2)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, HashSet::from(["unrelated.txt".into()]));
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_force_recheck_redownloads_corrupted_piece() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_recheck", 2).await;
    let (seed_session, _seed, addr) = seeder(&t).await;

    let root = TempDir::with_prefix("storage_e2e_recheck_client").unwrap();
    let out = root.path().join("out");
    let session = client_session(root.path(), Default::default(), None).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![addr]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;

    // Paused recheck: the corrupted piece is found missing, the torrent stays paused.
    session.pause(&h).await.unwrap();
    flip_byte(&out.join(t.name(1)), 1000);
    h.force_recheck().await.unwrap();
    wait_paused(&h).await;
    let needed = h.with_state(|s| match s {
        ManagedTorrentState::Paused(p) => p.chunk_tracker.get_hns().needed_bytes,
        _ => unreachable!(),
    });
    assert_eq!(needed, PIECE_LENGTH as u64);
    assert!(h.is_paused());

    session.unpause(&h).await.unwrap();
    wait_finished(&h).await;
    assert_eq!(
        std::fs::read(out.join(t.name(1))).unwrap(),
        t.original(t.name(1))
    );

    // Live recheck.
    flip_byte(&out.join(t.name(0)), 20_000);
    h.force_recheck().await.unwrap();
    wait_for("data to be repaired", || {
        Ok(std::fs::read(out.join(t.name(0)))? == t.original(t.name(0)))
    })
    .await;
    wait_finished(&h).await;
    assert!(!h.is_paused());

    session.stop().await;
    seed_session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_skip_hash_check_falls_back_to_missing_piece() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_skip", 2).await;
    let (seed_session, _seed, addr) = seeder(&t).await;

    // A copy of the data with one corrupted byte, added without hashing.
    let root = TempDir::with_prefix("storage_e2e_skip_client").unwrap();
    let out = root.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    for n in &t.names {
        std::fs::copy(t.dir.path().join(n), out.join(n)).unwrap();
    }
    flip_byte(&out.join(t.name(1)), 10);

    let session = client_session(root.path(), Default::default(), None).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![addr]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                overwrite: true,
                skip_hash_check: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;
    let metadata = h.metadata.load_full().unwrap();
    let lengths = *metadata.lengths();
    let total_pieces = lengths.total_pieces() as usize;
    assert_eq!(h.storage_layout().unverified_piece_count(), total_pieces);

    // Reading a good piece verifies it.
    let live = h.live().unwrap();
    let mut buf = vec![0u8; PIECE_LENGTH as usize];
    let good = lengths.validate_piece_index(0).unwrap();
    let chunk = lengths.iter_chunk_infos(good).next().unwrap();
    live.file_ops()
        .read_chunk(addr, &chunk, &mut buf[..chunk.size as usize])
        .unwrap();
    assert_eq!(
        h.storage_layout().unverified_piece_count(),
        total_pieces - 1
    );

    // Reading the corrupted piece fails and marks it missing; it's downloaded again.
    let file1 = &metadata.file_infos[1];
    let bad_piece = u32::try_from((file1.offset_in_torrent + 10) / PIECE_LENGTH as u64).unwrap();
    let bad = lengths.validate_piece_index(bad_piece).unwrap();
    let chunk = lengths.iter_chunk_infos(bad).next().unwrap();
    assert!(
        live.file_ops()
            .read_chunk(addr, &chunk, &mut buf[..chunk.size as usize])
            .is_err()
    );
    assert_eq!(
        h.storage_layout().unverified_piece_count(),
        total_pieces - 2
    );
    drop(live);
    wait_for("corrupted piece to be downloaded again", || {
        Ok(std::fs::read(out.join(t.name(1)))? == t.original(t.name(1)))
    })
    .await;
    wait_finished(&h).await;

    session.stop().await;
    seed_session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_incomplete_suffix_across_completion_and_restart() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_suffix", 3).await;
    let (seed_session, _seed, addr) = seeder(&t).await;

    let root = TempDir::with_prefix("storage_e2e_suffix_client").unwrap();
    let out = root.path().join("out");
    let persistence = root.path().join("session");
    let storage = StorageSettings {
        incomplete_suffix: Some(".!qB".into()),
        ..Default::default()
    };

    let session = client_session(root.path(), storage.clone(), Some(persistence.clone())).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![addr]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                only_files: Some(vec![0]),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let (n0, n1) = (t.name(0), t.name(1));
    wait_finished(&h).await;
    // File 0 is complete under its final name; file 1 got the shared piece's bytes
    // and is incomplete, so it carries the suffix.
    assert!(out.join(n0).exists());
    assert!(!out.join(format!("{n0}.!qB")).exists());
    assert!(out.join(format!("{n1}.!qB")).exists());
    assert!(!out.join(n1).exists());
    assert_eq!(std::fs::read(out.join(n0)).unwrap(), t.original(n0));
    assert_eq!(h.actual_file_paths()[1], PathBuf::from(format!("{n1}.!qB")));
    assert_eq!(h.file_paths()[1], PathBuf::from(n1));

    let id = h.id();
    drop(h);
    session.stop().await;
    drop(session);

    // Restart: the partially suffixed files are found, then the rest downloads.
    let session = client_session(root.path(), storage.clone(), Some(persistence)).await;
    let h = session.get(id.into()).unwrap();
    wait_finished(&h).await;
    assert_eq!(h.actual_file_paths()[1], PathBuf::from(format!("{n1}.!qB")));
    h.live().unwrap().add_peer_if_not_seen(addr).unwrap();
    session
        .update_only_files(&h, &HashSet::from([0, 1, 2]))
        .await
        .unwrap();
    wait_finished(&h).await;
    for n in &t.names {
        assert_eq!(std::fs::read(out.join(n)).unwrap(), t.original(n), "{n}");
        assert!(!out.join(format!("{n}.!qB")).exists());
    }

    // Turning the option off and on renames incomplete files only: none left.
    session
        .set_storage_settings(StorageSettings::default())
        .await
        .unwrap();
    assert!(out.join(t.name(2)).exists());

    session.stop().await;
    seed_session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_missing_files_detection_and_recovery() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_missing", 2).await;
    let (seed_session, _seed, addr) = seeder(&t).await;

    let root = TempDir::with_prefix("storage_e2e_missing_client").unwrap();
    let out = root.path().join("out");
    let moved = root.path().join("moved-away");
    let persistence = root.path().join("session");

    let session = client_session(root.path(), Default::default(), Some(persistence.clone())).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![addr]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_finished(&h).await;

    // Files vanish while paused: resuming reports missing files.
    session.pause(&h).await.unwrap();
    std::fs::rename(&out, &moved).unwrap();
    session.unpause(&h).await.unwrap();
    assert_eq!(h.error_kind(), Some(TorrentErrorKind::MissingFiles));

    let id = h.id();
    drop(h);
    session.stop().await;
    drop(session);

    // On restart (fastresume says we have data): missing files again, progress kept.
    let session = client_session(root.path(), Default::default(), Some(persistence)).await;
    let h = session.get(id.into()).unwrap();
    wait_for("missing files error", || {
        Ok(h.error_kind() == Some(TorrentErrorKind::MissingFiles))
    })
    .await;
    assert!(!out.exists());

    // Pointing the torrent at where the files are now recovers it.
    h.move_storage(moved.clone(), MoveMode::DontReplace)
        .await
        .unwrap();
    wait_finished(&h).await;
    assert_eq!(h.output_folder(), moved);
    assert_eq!(h.error_kind(), None);
    for n in &t.names {
        assert_eq!(std::fs::read(moved.join(n)).unwrap(), t.original(n));
    }
    info!("missing files recovered");

    session.stop().await;
    seed_session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_e2e_unwanted_folder_and_preallocation() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_unwanted", 3).await;
    let (seed_session, _seed, addr) = seeder(&t).await;

    let root = TempDir::with_prefix("storage_e2e_unwanted_client").unwrap();
    let out = root.path().join("out");
    let session = client_session(
        root.path(),
        StorageSettings {
            use_unwanted_folder: true,
            preallocate: true,
            file_pool_size: 1,
            ..Default::default()
        },
        None,
    )
    .await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                initial_peers: Some(vec![addr]),
                output_folder: Some(out.to_str().unwrap().to_owned()),
                only_files: Some(vec![0, 2]),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let n1 = t.name(1);
    wait_finished(&h).await;
    // The unwanted file got the bytes of shared pieces, in the unwanted folder.
    assert!(out.join(".unwanted").join(n1).exists());
    assert!(!out.join(n1).exists());
    assert_eq!(
        std::fs::metadata(out.join(t.name(0))).unwrap().len(),
        FILE_SIZE as u64
    );

    // Wanted again: moves back and completes.
    session
        .update_only_files(&h, &HashSet::from([0, 1, 2]))
        .await
        .unwrap();
    assert!(out.join(n1).exists());
    assert!(!out.join(".unwanted").exists());
    wait_finished(&h).await;
    for n in &t.names {
        assert_eq!(std::fs::read(out.join(n)).unwrap(), t.original(n), "{n}");
    }

    session.stop().await;
    seed_session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_max_active_checking_torrents_runtime() {
    setup_test_logging();
    let root = TempDir::with_prefix("storage_e2e_checking").unwrap();
    let session = client_session(root.path(), Default::default(), None).await;
    assert_eq!(session.max_active_checking_torrents(), 3);
    session.set_max_active_checking_torrents(-1);
    assert_eq!(session.max_active_checking_torrents(), -1);

    // With no slots, a torrent stays checking; giving it a slot lets it through.
    session.set_max_active_checking_torrents(0);
    let t = TestTorrent::new("storage_e2e_checking_t", 1).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                output_folder: Some(t.dir.path().to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(h.with_state(|s| matches!(s, ManagedTorrentState::Initializing(_))));
    session.set_max_active_checking_torrents(1);
    wait_paused(&h).await;
    let finished = h.with_state(|s| match s {
        ManagedTorrentState::Paused(p) => p.chunk_tracker.get_hns().finished(),
        _ => false,
    });
    assert!(finished);
    session.stop().await;
}

/// Adds `t` (paused) from its own folder with fastresume persistence in
/// `persistence`, waits for the initial check and stops the session.
async fn add_checked_and_stop(t: &TestTorrent, root: &Path, persistence: &Path) -> usize {
    let session = client_session(root, Default::default(), Some(persistence.to_owned())).await;
    let h = session
        .add_torrent(
            AddTorrent::from_bytes(t.bytes.clone()),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                output_folder: Some(t.dir.path().to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    wait_paused(&h).await;
    assert!(paused_have_bytes(&h) > 0);
    let id = h.id();
    drop(h);
    session.stop().await;
    id
}

/// Restarts a session on `persistence` and returns the torrent's have bytes once
/// its fastresume data was validated.
async fn restart_have_bytes(root: &Path, persistence: &Path, id: usize) -> u64 {
    let session = client_session(root, Default::default(), Some(persistence.to_owned())).await;
    let h = session.get(id.into()).unwrap();
    wait_paused(&h).await;
    let have = paused_have_bytes(&h);
    drop(h);
    session.stop().await;
    have
}

fn paused_have_bytes(h: &ManagedTorrentHandle) -> u64 {
    h.with_state(|s| match s {
        ManagedTorrentState::Paused(p) => p.chunk_tracker.get_hns().have_bytes,
        _ => unreachable!(),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn test_fastresume_sampling_detects_corrupted_piece() {
    setup_test_logging();
    let t = TestTorrent::new("storage_e2e_fastresume_corrupt", 2).await;
    let total = (FILE_SIZE * 2) as u64;
    let root = TempDir::with_prefix("storage_e2e_fastresume_corrupt_client").unwrap();
    let persistence = root.path().join("session");
    let id = add_checked_and_stop(&t, root.path(), &persistence).await;

    // The first piece of every file is always among the sampled ones: corrupting
    // it must invalidate the fastresume data (full recheck finds the bad piece).
    flip_byte(&t.dir.path().join(t.name(0)), 100);
    let have = restart_have_bytes(root.path(), &persistence, id).await;
    assert_eq!(have, total - PIECE_LENGTH as u64);
}
