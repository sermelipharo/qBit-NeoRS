//! Acceptance: two cores on localhost, one seeds, one downloads.

use std::time::Duration;

use qbt_core::model::{AddTorrentParams, FilePriority, TorrentSource};
use qbt_e2e::*;

#[test]
fn seed_and_download_over_localhost() {
    let seeder = Node::start(free_port(), |_| {});
    let leecher = Node::start(free_port(), |_| {});

    let content = make_content(
        &seeder.downloads(),
        "Sample Collection",
        &[("a/one.bin", 3 * 1024 * 1024 + 17), ("b/two.bin", 700_000), ("three.txt", 4096)],
    );
    let torrent = make_torrent(&content, 256 * 1024);

    let rt = seeder.app.runtime();
    let hash = rt
        .block_on(seeder.app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(seeder.downloads().to_string_lossy().into()), ..Default::default() },
        ))
        .expect("seeder add");
    seeder.wait_for(&hash, Duration::from_secs(30), "seeder to finish checking", is_seeding);

    let hash2 = leecher
        .app
        .runtime()
        .block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default()))
        .expect("leecher add");
    assert_eq!(hash, hash2);
    leecher.app.add_peers(vec![hash.clone()], vec![format!("127.0.0.1:{}", seeder.port)]).expect("add_peers");

    let done = leecher.wait_for(&hash, Duration::from_secs(60), "download to complete", is_seeding);
    assert!(done.completion_on > 0);
    assert!(same_tree(&content, &leecher.downloads().join("Sample Collection")));

    // The seeder saw the upload.
    let s = seeder.torrent(&hash).unwrap();
    assert!(s.uploaded > 0, "seeder uploaded {}", s.uploaded);
}

#[test]
fn skipped_files_are_not_downloaded() {
    let seeder = Node::start(free_port(), |_| {});
    let leecher = Node::start(free_port(), |_| {});
    let content = make_content(&seeder.downloads(), "Partial", &[("keep.bin", 1_500_000), ("skip.bin", 1_500_000)]);
    let torrent = make_torrent(&content, 64 * 1024);
    let hash = seeder
        .app
        .runtime()
        .block_on(seeder.app.add_torrent(TorrentSource::Bytes { data: torrent.clone() }, AddTorrentParams::default()))
        .unwrap();
    seeder.wait_for(&hash, Duration::from_secs(30), "seeder ready", is_seeding);

    // The engine's creator does not sort files, so pick priorities by name.
    let meta = leecher
        .app
        .runtime()
        .block_on(leecher.app.fetch_metadata(TorrentSource::Bytes { data: torrent.clone() }))
        .unwrap();
    let skip = |p: &str| p.ends_with("skip.bin");
    let params = AddTorrentParams {
        file_priorities: meta
            .files
            .iter()
            .map(|f| if skip(&f.path) { FilePriority::Ignored } else { FilePriority::Normal })
            .collect(),
        ..Default::default()
    };
    leecher.app.runtime().block_on(leecher.app.add_torrent(TorrentSource::Bytes { data: torrent }, params)).unwrap();
    leecher.app.add_peers(vec![hash.clone()], vec![format!("127.0.0.1:{}", seeder.port)]).unwrap();
    leecher.wait_for(&hash, Duration::from_secs(60), "wanted files complete", |t| (t.progress - 1.0).abs() < 1e-9);

    let files = leecher.app.files(hash.clone(), vec![]).unwrap();
    assert_eq!(files.len(), 2);
    let skipped = files.iter().find(|f| skip(&f.name)).unwrap();
    assert_eq!(skipped.priority, FilePriority::Ignored);
    let kept = std::fs::read(leecher.downloads().join("Partial/keep.bin")).unwrap();
    assert_eq!(kept, std::fs::read(content.join("keep.bin")).unwrap());
}

#[test]
fn state_survives_restart() {
    let port = free_port();
    let node = Node::start(port, |_| {});
    let content = make_content(&node.downloads(), "Persist", &[("x.bin", 300_000)]);
    let torrent = make_torrent(&content, 32 * 1024);
    let app = node.app.clone();
    let hash = app
        .runtime()
        .block_on(app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default()))
        .unwrap();
    node.wait_for(&hash, Duration::from_secs(30), "seeding", is_seeding);
    app.create_category(qbt_core::model::Category { name: "Linux".into(), ..Default::default() }).unwrap();
    app.set_category(vec![hash.clone()], "Linux".into()).unwrap();
    app.add_tags(vec![hash.clone()], vec!["iso".into()]).unwrap();
    app.runtime().block_on(app.stop_torrents(vec![hash.clone()])).unwrap();
    app.shutdown();
    drop(app);

    // Restart on the same profile.
    let profile = node.dir.path().join("profile");
    let app2 = qbt_core::App::new(qbt_core::AppConfig {
        profile_dir: profile.to_string_lossy().into_owned(),
        ..Default::default()
    })
    .unwrap();
    let t = app2.torrent(hash.clone()).expect("torrent restored");
    assert_eq!(t.category, "Linux");
    assert_eq!(t.tags, vec!["iso".to_string()]);
    assert!(t.state.is_stopped(), "state {:?}", t.state);
    app2.shutdown();
    std::mem::forget(node); // its App was already shut down above
}
