use std::time::Duration;

use anyhow::Context;
use tempfile::TempDir;

use crate::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, CreateTorrentOptions, Session,
    SessionOptions, SessionPersistenceConfig, create_torrent, spawn_utils::BlockingSpawner,
    tests::test_util::{create_default_random_dir_with_torrents, setup_test_logging},
};

// Torrents added at the same time each get their own ID and handle. The persistence store used
// to hand out max(stored id) + 1, and a torrent is stored only after it is added, so concurrent
// adds got the same ID and all but the first were answered with the first torrent's handle.
#[tokio::test(flavor = "multi_thread")]
async fn test_concurrent_adds_get_their_own_torrents() -> anyhow::Result<()> {
    setup_test_logging();
    const N: usize = 16;
    let mut torrents = Vec::new();
    let mut dirs = Vec::new();
    for i in 0..N {
        let dir = create_default_random_dir_with_torrents(1, 4096, Some(&format!("rqbit_add_{i}")));
        let t = create_torrent(dir.path(), CreateTorrentOptions::default(), &BlockingSpawner::new(1))
            .await?;
        torrents.push((t.info_hash(), t.as_bytes()?));
        dirs.push(dir);
    }
    let out = TempDir::with_prefix("rqbit_add_out")?;
    let session = Session::new_with_opts(
        out.path().into(),
        SessionOptions {
            dht: None,
            persistence: Some(SessionPersistenceConfig::FastResumeOnly {
                folder: out.path().join("resume"),
            }),
            listen: None,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .await
    .context("session")?;

    let adds = torrents.iter().zip(&dirs).map(|((_, bytes), dir)| {
        let session = session.clone();
        let bytes = bytes.clone();
        let folder = dir.path().to_str().unwrap().to_owned();
        tokio::spawn(async move {
            session
                .add_torrent(
                    AddTorrent::from_bytes(bytes),
                    Some(AddTorrentOptions {
                        paused: true,
                        output_folder: Some(folder),
                        overwrite: true,
                        ..Default::default()
                    }),
                )
                .await
        })
    });
    let results = tokio::time::timeout(Duration::from_secs(30), futures::future::join_all(adds))
        .await
        .context("adds timed out")?;

    let mut ids = std::collections::HashSet::new();
    for ((info_hash, _), r) in torrents.iter().zip(results) {
        match r?? {
            AddTorrentResponse::Added(id, h) => {
                assert_eq!(h.info_hash(), *info_hash, "handle of another torrent");
                assert!(ids.insert(id), "id {id} given twice");
            }
            other => panic!("not added: {:?}", other.into_handle().map(|h| h.info_hash())),
        }
    }
    session.stop().await;
    Ok(())
}
