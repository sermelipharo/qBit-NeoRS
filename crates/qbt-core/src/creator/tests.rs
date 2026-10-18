use std::time::{Duration, Instant};

use super::*;
use crate::net::testutil;

fn manager(rt: &tokio::runtime::Runtime, dir: &Path) -> Arc<CreatorManager> {
    let ctx = testutil::context(rt, dir);
    rt.block_on(async {
        let session = Session::launch(&ctx).await.unwrap();
        CreatorManager::launch(&ctx, session).await.unwrap()
    })
}

fn wait_finished(m: &CreatorManager, id: &str) -> CreatorTask {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let t = m.task(id).unwrap();
        if matches!(t.status, CreatorTaskStatus::Finished | CreatorTaskStatus::Failed) {
            return t;
        }
        assert!(Instant::now() < deadline, "task did not finish: {t:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn params(source: &Path, out: &Path) -> TorrentCreatorParams {
    TorrentCreatorParams {
        source_path: source.to_string_lossy().into_owned(),
        torrent_file_path: out.to_string_lossy().into_owned(),
        format: TorrentFormat::Hybrid,
        padding_file_limit: -1,
        trackers: vec!["http://tracker/announce".into()],
        ..Default::default()
    }
}

#[test]
fn creates_and_serves_torrent() {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&rt, &dir.path().join("profile"));
    let src = dir.path().join("data");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("one.bin"), vec![7u8; 100_000]).unwrap();
    std::fs::write(src.join("two.bin"), vec![9u8; 5_000]).unwrap();
    let out = dir.path().join("out/data.torrent");

    let mut events = m.ctx.events.subscribe();
    let id = m.add_task(params(&src, &out)).unwrap();
    assert_eq!(id.len(), 36);
    assert_eq!(&id[14..15], "4");
    let t = wait_finished(&m, &id);
    assert_eq!(t.status, CreatorTaskStatus::Finished, "{t:?}");
    assert_eq!(t.progress, 100.0);
    assert_eq!(t.piece_size, 16 * 1024);
    assert!(t.time_added > 0 && t.time_started >= t.time_added && t.time_finished >= t.time_started);
    assert_eq!(t.info_hash.len(), 40);
    assert_eq!(t.torrent_file_path, out.to_string_lossy());

    let bytes = m.torrent_file(&id).unwrap();
    assert_eq!(bytes, std::fs::read(&out).unwrap());
    let parsed = librqbit::torrent_from_bytes(&bytes).unwrap();
    // A hybrid torrent (the default format): its id is the truncated v2 hash.
    assert_eq!(parsed.info_hash_v2.unwrap().truncate_for_dht().as_string(), t.info_hash);
    assert!(events.try_recv().is_ok());

    assert_eq!(m.tasks().len(), 1);
    m.delete_task(&id).unwrap();
    assert!(m.task(&id).is_err());
    assert!(matches!(m.delete_task(&id), Err(QbtError::NotFound { .. })));
    // A user-chosen output file stays.
    assert!(out.exists());
}

#[test]
fn temp_output_and_failures() {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let m = manager(&rt, &profile);
    let file = dir.path().join("single.iso");
    std::fs::write(&file, vec![1u8; 70_000]).unwrap();

    let mut p = params(&file, Path::new(""));
    p.format = TorrentFormat::V2;
    let id = m.add_task(p).unwrap();
    let t = wait_finished(&m, &id);
    assert_eq!(t.status, CreatorTaskStatus::Finished, "{t:?}");
    let temp = profile.join("cache/torrent_creator").join(format!("{id}.torrent"));
    assert!(temp.exists());
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&temp).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(!m.torrent_file(&id).unwrap().is_empty());
    m.delete_task(&id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while temp.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!temp.exists());

    // Missing source: the task fails with qBittorrent's message.
    let id = m.add_task(params(&dir.path().join("missing"), Path::new(""))).unwrap();
    let t = wait_finished(&m, &id);
    assert_eq!(t.status, CreatorTaskStatus::Failed);
    assert_eq!(t.error_message, "Create new torrent file failed. Reason: Path to file/folder is not readable.");
    assert!(matches!(m.torrent_file(&id), Err(QbtError::Conflict { .. })));

    // Invalid arguments are rejected up front.
    assert!(m.add_task(params(Path::new("relative/path"), Path::new(""))).is_err());
    assert!(m.add_task(params(&file, &dir.path().join(".bashrc"))).is_err());
    assert!(m.add_task(params(&file, Path::new("out.torrent"))).is_err());
}

#[test]
fn task_limit_evicts_finished() {
    let rt = testutil::runtime();
    let dir = tempfile::tempdir().unwrap();
    let m = manager(&rt, &dir.path().join("profile"));
    let missing = dir.path().join("missing");
    let mut ids = Vec::new();
    for _ in 0..MAX_TASKS {
        ids.push(m.add_task(params(&missing, Path::new(""))).unwrap());
    }
    for id in &ids {
        wait_finished(&m, id);
    }
    let newest = m.add_task(params(&missing, Path::new(""))).unwrap();
    assert_eq!(m.tasks().len(), MAX_TASKS);
    assert!(m.task(&ids[0]).is_err(), "the oldest finished task is evicted");
    assert!(m.task(&newest).is_ok());
}

#[test]
fn calculates_pieces() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("d");
    std::fs::create_dir(&src).unwrap();
    std::fs::write(src.join("a"), vec![0u8; 20_000]).unwrap();
    std::fs::write(src.join("b"), vec![0u8; 20_000]).unwrap();
    // v1: 40000 bytes / 16 KiB = 3 pieces; hybrid pads each file to 2 pieces.
    assert_eq!(CreatorManager::calculate_total_pieces(&src, 16384, TorrentFormat::V1).unwrap(), 3);
    assert_eq!(CreatorManager::calculate_total_pieces(&src, 16384, TorrentFormat::Hybrid).unwrap(), 4);
    assert_eq!(auto_piece_size(1 << 30, TorrentFormat::V1), 512 * 1024);
}
