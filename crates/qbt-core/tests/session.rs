//! Integration tests of the BitTorrent session through the `App` facade:
//! a temporary profile, no DHT/LSD/UPnP, torrents created from local data.
//!
//! These are plain `#[test]`s: `App` owns its own Tokio runtime and blocks on
//! it, which is not allowed from inside another runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qbt_core::model::*;
use qbt_core::prefs::Preferences;
use qbt_core::{App, AppConfig, QbtError, QbtResult};

fn write_prefs(profile: &Path, downloads: &Path) {
    let mut p = Preferences::default();
    p.downloads.save_path = downloads.to_string_lossy().into_owned();
    p.downloads.temp_path = downloads.join("temp").to_string_lossy().into_owned();
    p.connection.listen_port = 0;
    p.connection.upnp = false;
    p.bittorrent.dht = false;
    p.bittorrent.lsd = false;
    p.bittorrent.pex = false;
    p.behavior.file_log_enabled = false;
    p.advanced.resolve_peer_countries = false;
    p.advanced.refresh_interval = 250;
    std::fs::create_dir_all(profile.join("config")).unwrap();
    std::fs::write(profile.join("config/preferences.json"), serde_json::to_vec_pretty(&p).unwrap()).unwrap();
}

fn start(profile: &Path) -> Arc<App> {
    App::new(AppConfig { profile_dir: profile.to_string_lossy().into_owned(), front_end: "test".into(), ..Default::default() })
        .expect("App::new")
}

fn make_data(root: &Path, name: &str, files: &[(&str, usize)]) -> PathBuf {
    let base = root.join(name);
    for (i, (rel, size)) in files.iter().enumerate() {
        let p = base.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let data: Vec<u8> = (0..*size).map(|j| ((j * 31 + i * 7) % 251) as u8).collect();
        std::fs::write(p, data).unwrap();
    }
    base
}

fn make_torrent(path: &Path) -> Vec<u8> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
        let t = librqbit::create_torrent(
            path,
            librqbit::CreateTorrentOptions {
                name: None,
                trackers: vec!["http://127.0.0.1:1/announce".into()],
                piece_length: Some(32 * 1024),
            },
            &spawner,
        )
        .await
        .unwrap();
        t.as_bytes().unwrap().to_vec()
    })
}

fn wait_for(app: &App, hash: &str, what: &str, pred: impl Fn(&TorrentInfo) -> bool) -> TorrentInfo {
    let start = Instant::now();
    loop {
        if let Ok(t) = app.torrent(hash.to_string())
            && pred(&t)
        {
            return t;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}: {:?}", app.torrent(hash.to_string()));
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn seeding(t: &TorrentInfo) -> bool {
    t.state == TorrentState::StalledUploading && t.progress == 1.0
}

#[test]
fn torrent_lifecycle_and_persistence() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = make_data(&downloads, "Album", &[("01.bin", 100_000), ("sub/02.bin", 70_000)]);
    let torrent = make_torrent(&data);

    let app = start(&profile);
    let rt = app.runtime();
    let hash = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent.clone() },
            AddTorrentParams { save_path: Some(downloads.to_string_lossy().into()), ..Default::default() },
        ))
        .expect("add");
    let t = wait_for(&app, &hash, "seeding after check", seeding);
    assert_eq!(t.size, 170_000);
    assert_eq!(t.total_size, 170_000);
    assert_eq!(t.name, "Album");
    assert_eq!(t.content_path, data.to_string_lossy());
    assert_eq!(t.root_path, data.to_string_lossy());
    assert_eq!(t.trackers_count, 1);
    assert!(t.completion_on > 0);
    assert_eq!(t.amount_left, 0);
    assert!(t.magnet_uri.starts_with(&format!("magnet:?xt=urn:btih:{hash}")));

    // Properties tabs.
    let files = app.files(hash.clone(), vec![]).unwrap();
    assert_eq!(files.len(), 2);
    assert!(files.iter().all(|f| f.progress == 1.0));
    let trackers = app.trackers(hash.clone()).unwrap();
    assert_eq!(trackers.len(), 4);
    assert_eq!(trackers[0].url, "** [DHT] **");
    assert_eq!(trackers[0].status, TrackerStatus::Disabled);
    assert_eq!(trackers[3].url, "http://127.0.0.1:1/announce");
    let pieces = app.piece_states(hash.clone()).unwrap();
    assert!(!pieces.is_empty() && pieces.iter().all(|&p| p == 2));
    assert_eq!(app.piece_hashes(hash.clone()).unwrap().len(), pieces.len());
    let props = app.properties(hash.clone()).unwrap();
    assert_eq!(props.total_size, 170_000);
    assert_eq!(props.save_path, downloads.to_string_lossy());
    let exported = app.export_torrent(hash.clone()).unwrap();
    let meta = rt.block_on(app.fetch_metadata(TorrentSource::Bytes { data: exported })).unwrap();
    assert_eq!(meta.hash, hash);
    assert!(meta.is_duplicate);

    // Duplicate add is refused.
    let dup = rt.block_on(app.add_torrent(TorrentSource::Bytes { data: torrent.clone() }, AddTorrentParams::default()));
    assert!(matches!(dup, Err(QbtError::Conflict { .. })), "{dup:?}");

    // Stop / start.
    rt.block_on(app.stop_torrents(vec![hash.clone()])).unwrap();
    let t = wait_for(&app, &hash, "stopped", |t| t.state == TorrentState::StoppedUploading);
    assert_eq!(t.eta, MAX_ETA);
    assert_eq!(app.torrents_filtered(TorrentQuery { status: StatusFilter::Stopped, ..Default::default() }).len(), 1);
    assert_eq!(app.torrents_filtered(TorrentQuery { status: StatusFilter::Running, ..Default::default() }).len(), 0);
    rt.block_on(app.start_torrents(vec![hash.clone()])).unwrap();
    wait_for(&app, &hash, "seeding again", seeding);
    rt.block_on(app.set_force_start(vec![hash.clone()], true)).unwrap();
    wait_for(&app, &hash, "forced", |t| t.state == TorrentState::ForcedUploading && t.force_start);
    rt.block_on(app.set_force_start(vec![hash.clone()], false)).unwrap();

    // Categories (with subcategories), tags, name, limits.
    app.create_category(Category { name: "Linux/ISO".into(), ..Default::default() }).unwrap();
    let names: Vec<String> = app.categories().into_iter().map(|c| c.name).collect();
    assert_eq!(names, vec!["Linux".to_string(), "Linux/ISO".to_string()]);
    assert!(app.create_category(Category { name: "bad//name".into(), ..Default::default() }).is_err());
    app.set_category(vec![hash.clone()], "Linux/ISO".into()).unwrap();
    assert!(app.set_category(vec![hash.clone()], "Missing".into()).is_err());
    app.add_tags(vec![hash.clone()], vec!["iso".into(), "x86".into()]).unwrap();
    assert!(app.add_tags(vec![hash.clone()], vec!["a,b".into()]).is_err());
    app.rename_torrent(hash.clone(), "My\nAlbum".into()).unwrap();
    app.set_share_limits(vec![hash.clone()], 2.5, -2, -1, ShareLimitAction::Stop).unwrap();
    let q = TorrentQuery { category: Some("Linux".into()), tag: Some("iso".into()), ..Default::default() };
    assert_eq!(app.torrents_filtered(q).len(), 1);

    // Rename a file inside the torrent (on disk too).
    rt.block_on(app.rename_file(hash.clone(), "Album/01.bin".into(), "Album/first.bin".into())).unwrap();
    assert!(downloads.join("Album/first.bin").exists());
    assert!(!downloads.join("Album/01.bin").exists());
    let err = rt.block_on(app.rename_file(hash.clone(), "Album/first.bin".into(), "../escape.bin".into()));
    assert!(err.is_err());
    let err = rt.block_on(app.rename_folder(hash.clone(), "Album/sub".into(), "Album/first.bin/x".into()));
    assert!(err.is_err(), "a file cannot become a folder");
    rt.block_on(app.rename_folder(hash.clone(), "Album/sub".into(), "Album/Sub2".into())).unwrap();
    assert!(downloads.join("Album/Sub2/02.bin").exists());

    // Move the data.
    let new_location = tmp.path().join("moved");
    rt.block_on(app.set_location(vec![hash.clone()], new_location.to_string_lossy().into())).unwrap();
    let t = wait_for(&app, &hash, "moved and seeding", |t| seeding(t) && t.save_path == new_location.to_string_lossy());
    assert!(!t.auto_tmm);
    assert!(new_location.join("Album/first.bin").exists());
    assert!(new_location.join("Album/Sub2/02.bin").exists());
    assert!(!data.exists(), "source folder cleaned up");

    // Recheck keeps everything complete.
    rt.block_on(app.recheck_torrents(vec![hash.clone()])).unwrap();
    wait_for(&app, &hash, "seeding after recheck", seeding);

    rt.block_on(app.stop_torrents(vec![hash.clone()])).unwrap();
    app.shutdown();
    drop(app);

    // Restart: everything persisted.
    let app = start(&profile);
    let t = app.torrent(hash.clone()).expect("restored");
    assert_eq!(t.name, "My Album");
    assert_eq!(t.category, "Linux/ISO");
    assert_eq!(t.tags, vec!["iso".to_string(), "x86".to_string()]);
    assert_eq!(t.ratio_limit, 2.5);
    assert_eq!(t.inactive_seeding_time_limit, -1);
    assert!(t.state.is_stopped(), "{:?}", t.state);
    assert_eq!(t.save_path, new_location.to_string_lossy());
    assert_eq!(app.tags(), vec!["iso".to_string(), "x86".to_string()]);
    assert_eq!(app.categories().len(), 2);
    let files = app.files(hash.clone(), vec![]).unwrap();
    assert!(files.iter().any(|f| f.name == "Album/first.bin"));
    assert!(files.iter().any(|f| f.name == "Album/Sub2/02.bin"));
    let t = wait_for(&app, &hash, "checked after restart", |t| t.state == TorrentState::StoppedUploading && t.progress == 1.0);
    assert!(t.uploaded >= 0);

    // Remove with content.
    let rt = app.runtime();
    rt.block_on(app.remove_torrents(vec![hash.clone()], true)).unwrap();
    assert!(app.torrent(hash.clone()).is_err());
    assert!(!new_location.join("Album").exists());
    // Categories are untouched by removal.
    app.remove_categories(vec!["Linux".into()]).unwrap();
    assert!(app.categories().is_empty());
    app.shutdown();
    drop(app);
    let app = start(&profile);
    assert_eq!(app.torrent_count(), 0);
    app.shutdown();
}

#[test]
fn magnets_and_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let app = start(&profile);
    let rt = app.runtime();

    let h = "0123456789abcdef0123456789abcdef01234567";
    let magnet = format!("magnet:?xt=urn:btih:{h}&dn=Some+Name&tr=udp%3A%2F%2Ftracker.example%3A80");
    let added = rt
        .block_on(app.add_torrent(TorrentSource::Url { url: magnet }, AddTorrentParams { stopped: Some(true), ..Default::default() }))
        .unwrap();
    assert_eq!(added, h);
    let t = app.torrent(h.into()).unwrap();
    assert_eq!(t.name, "Some Name");
    assert!(!t.has_metadata);
    assert_eq!(t.state, TorrentState::StoppedDownloading);
    assert_eq!(t.private, None);
    assert_eq!(t.total_size, -1);
    assert_eq!(app.files(h.into(), vec![]).unwrap(), vec![]);
    assert!(app.export_torrent(h.into()).is_err());
    rt.block_on(app.start_torrents(vec![h.into()])).unwrap();
    wait_for(&app, h, "metadata download", |t| t.state == TorrentState::DownloadingMetadata);

    // Bare hashes in base32, v2 hashes, unsupported schemes, garbage.
    let b32 = rt.block_on(app.add_torrent(
        TorrentSource::Url { url: "AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH".into() },
        AddTorrentParams::default(),
    ));
    assert!(matches!(b32, Err(QbtError::Conflict { .. })), "same hash as the magnet: {b32:?}");
    // A bare v2 hash is a v2-only magnet: its id is the truncated hash.
    let v2 = rt
        .block_on(app.add_torrent(
            TorrentSource::Url { url: "a".repeat(64) },
            AddTorrentParams { stopped: Some(true), ..Default::default() },
        ))
        .unwrap();
    assert_eq!(v2, "a".repeat(40));
    let t = app.torrent(v2.clone()).unwrap();
    assert_eq!((t.infohash_v1.as_str(), t.infohash_v2.as_str()), ("", "a".repeat(64).as_str()));
    assert_eq!(t.magnet_uri, format!("magnet:?xt=urn:btmh:1220{}", "a".repeat(64)));
    assert!(rt.block_on(app.add_torrent(TorrentSource::Url { url: "g".repeat(64) }, AddTorrentParams::default())).is_err());
    let file_url =
        rt.block_on(app.add_torrent(TorrentSource::Url { url: "file:///etc/passwd".into() }, AddTorrentParams::default()));
    assert!(file_url.is_err());
    let garbage = rt.block_on(app.add_torrent(TorrentSource::Bytes { data: b"not a torrent".to_vec() }, AddTorrentParams::default()));
    assert!(garbage.is_err());
    let missing = rt.block_on(app.add_torrent(
        TorrentSource::File { path: tmp.path().join("nope.torrent").to_string_lossy().into() },
        AddTorrentParams::default(),
    ));
    assert!(missing.is_err());

    // A torrent with a category created on the fly and automatic mode.
    let data = make_data(&tmp.path().join("src"), "One", &[("f.bin", 5000)]);
    let torrent = make_torrent(&data.join("f.bin"));
    let id = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent },
            AddTorrentParams {
                category: Some("New/Sub".into()),
                auto_tmm: Some(true),
                stopped: Some(true),
                tags: vec!["t1".into(), "".into()],
                ..Default::default()
            },
        ))
        .unwrap();
    let t = app.torrent(id.clone()).unwrap();
    assert!(t.auto_tmm);
    assert_eq!(t.save_path, downloads.join("New/Sub").to_string_lossy());
    assert_eq!(t.tags, vec!["t1".to_string()]);
    assert_eq!(app.categories().len(), 2);
    // Switching off ATM freezes the category path.
    app.set_auto_management(vec![id.clone()], false).unwrap();
    assert_eq!(app.torrent(id.clone()).unwrap().save_path, downloads.join("New/Sub").to_string_lossy());

    rt.block_on(app.remove_torrents(vec![h.into(), id, v2], false)).unwrap();
    assert_eq!(app.torrent_count(), 0);
    app.shutdown();
}

#[test]
fn add_options_queueing_and_share_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let app = start(&profile);
    let rt = app.runtime();

    // Excluded file names and content layout.
    let mut prefs = app.preferences();
    prefs.downloads.excluded_file_names_enabled = true;
    prefs.downloads.excluded_file_names = vec!["*.nfo".into()];
    app.set_preferences(prefs).unwrap();
    let src = make_data(&tmp.path().join("src"), "Pack", &[("a.bin", 40_000), ("info.nfo", 100)]);
    let torrent = make_torrent(&src);
    let id = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent },
            AddTorrentParams { stopped: Some(true), content_layout: Some(ContentLayout::NoSubfolder), ..Default::default() },
        ))
        .unwrap();
    let files = app.files(id.clone(), vec![]).unwrap();
    let nfo = files.iter().find(|f| f.name == "info.nfo").expect("root folder stripped");
    assert_eq!(nfo.priority, FilePriority::Ignored);
    assert!(files.iter().any(|f| f.name == "a.bin" && f.priority == FilePriority::Normal));
    let t = app.torrent(id.clone()).unwrap();
    assert_eq!(t.size, 40_000);
    assert_eq!(t.state, TorrentState::StoppedDownloading);
    app.set_file_priority(id.clone(), vec![1], FilePriority::High).unwrap();
    assert!(app.set_file_priority(id.clone(), vec![9], FilePriority::High).is_err());
    rt.block_on(app.remove_torrents(vec![id], false)).unwrap();

    // Stop condition "files checked" on existing data.
    let data = make_data(&downloads, "Checked", &[("x.bin", 50_000)]);
    let torrent = make_torrent(&data);
    let id = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent },
            AddTorrentParams { stop_condition: Some(StopCondition::FilesChecked), ..Default::default() },
        ))
        .unwrap();
    wait_for(&app, &id, "stopped after checking", |t| t.state == TorrentState::StoppedUploading && t.progress == 1.0);

    // Share limits: ratio 0 is reached as soon as the torrent seeds.
    rt.block_on(app.start_torrents(vec![id.clone()])).unwrap();
    app.set_share_limits(vec![id.clone()], 0.0, -2, -2, ShareLimitAction::Stop).unwrap();
    let start_wait = Instant::now();
    loop {
        let t = app.torrent(id.clone()).unwrap();
        if t.state == TorrentState::StoppedUploading {
            break;
        }
        assert!(start_wait.elapsed() < Duration::from_secs(20), "share limit not applied: {:?}", t.state);
        std::thread::sleep(Duration::from_millis(200));
    }
    rt.block_on(app.remove_torrents(vec![id], false)).unwrap();

    // Queueing: one active download, magnets wait in the queue.
    let mut prefs = app.preferences();
    prefs.bittorrent.queueing_enabled = true;
    prefs.bittorrent.max_active_downloads = 1;
    app.set_preferences(prefs).unwrap();
    let a = "1111111111111111111111111111111111111111";
    let b = "2222222222222222222222222222222222222222";
    for h in [a, b] {
        rt.block_on(app.add_torrent(TorrentSource::Url { url: h.into() }, AddTorrentParams::default())).unwrap();
    }
    wait_for(&app, b, "second queued", |t| t.state == TorrentState::QueuedDownloading && t.queue_position == 2);
    assert_eq!(app.torrent(a.into()).unwrap().state, TorrentState::DownloadingMetadata);
    app.queue_top(vec![b.into()]).unwrap();
    wait_for(&app, a, "first queued after reorder", |t| t.state == TorrentState::QueuedDownloading);
    assert_eq!(app.torrent(b.into()).unwrap().queue_position, 1);
    rt.block_on(app.set_force_start(vec![a.into()], true)).unwrap();
    wait_for(&app, a, "forced ignores the queue", |t| t.state == TorrentState::ForcedDownloadingMetadata);
    app.shutdown();
}

fn read_resume(profile: &Path, id: &str) -> serde_json::Value {
    let p = profile.join(format!("data/BT_backup/{id}.fastresume"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn trusted_urls(v: &serde_json::Value) -> Vec<String> {
    v["trusted_urls"].as_array().unwrap().iter().map(|u| u.as_str().unwrap().to_string()).collect()
}

/// SSRF mitigation: only trackers and web seeds of a local .torrent file and
/// those the user adds are trusted; magnets, URLs, RSS and search plugins are
/// remote sources. The trust is persisted in the resume data.
#[test]
fn tracker_trust_follows_the_torrent_source() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let app = start(&profile);
    let rt = app.runtime();

    let h = "0123456789abcdef0123456789abcdef01234567";
    let magnet = format!("magnet:?xt=urn:btih:{h}&tr=http%3A%2F%2F192.168.1.1%2Fannounce%3Fk%3Dv&ws=http%3A%2F%2F10.0.0.1%2Ff%3Fx");
    rt.block_on(app.add_torrent(TorrentSource::Url { url: magnet }, AddTorrentParams { stopped: Some(true), ..Default::default() }))
        .unwrap();
    assert!(trusted_urls(&read_resume(&profile, h)).is_empty());

    let data = make_data(&tmp.path().join("src"), "Local", &[("f.bin", 5000)]);
    let id = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: make_torrent(&data.join("f.bin")) },
            AddTorrentParams { stopped: Some(true), ..Default::default() },
        ))
        .unwrap();
    assert_eq!(trusted_urls(&read_resume(&profile, &id)), vec!["http://127.0.0.1:1/announce".to_string()]);

    // User additions are trusted, on any torrent.
    app.add_trackers(h.into(), vec![TrackerTierEntry { url: "http://10.1.1.1/announce?u=1".into(), tier: 0 }]).unwrap();
    app.add_web_seeds(h.into(), vec!["http://10.1.1.2/f?u=1".into()]).unwrap();
    app.shutdown();
    let urls = trusted_urls(&read_resume(&profile, h));
    assert_eq!(urls, vec!["http://10.1.1.1/announce?u=1".to_string(), "http://10.1.1.2/f?u=1".to_string()]);
}

/// A proxy that can't be applied live must not leave the session running on
/// direct connections: the engine is restarted, and a session that can't use
/// the proxy does not start at all.
#[test]
fn unusable_proxy_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let app = start(&profile);

    let mut p = app.preferences();
    p.connection.proxy_type = qbt_core::prefs::ProxyType::Http;
    p.connection.proxy_ip = "proxy.example.com:1080".into();
    p.connection.proxy_port = 1080;
    p.connection.proxy_bittorrent = true;
    p.connection.proxy_peer_connections = true;
    app.set_preferences(p).unwrap();

    let deadline = Instant::now() + Duration::from_secs(20);
    let logged = |needle: &str| app.main_log(-1, vec![]).iter().any(|m| m.message.contains(needle));
    while !(logged("Failed to apply the network settings") && logged("serious error")) {
        assert!(Instant::now() < deadline, "no fail-closed restart: {:#?}", app.main_log(-1, vec![]));
        std::thread::sleep(Duration::from_millis(100));
    }
    app.shutdown();
}

/// The same torrent with `private=1` in its info dict (BEP 27): "private"
/// sorts after "pieces", the info dict's last key, and "info" is the
/// torrent's last key.
fn make_private(torrent: &[u8]) -> Vec<u8> {
    assert!(torrent.ends_with(b"ee"));
    let mut v = torrent[..torrent.len() - 2].to_vec();
    v.extend_from_slice(b"7:privatei1eee");
    v
}

/// Per-torrent "Disable DHT/PeX/LSD for this torrent": applied, shown in the
/// Trackers tab, persisted, and ignored for private torrents.
#[test]
fn per_torrent_discovery_switches() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let public = make_data(&downloads, "Public", &[("p.bin", 40_000)]);
    let private = make_data(&downloads, "Private", &[("q.bin", 40_000)]);
    let app = start(&profile);
    let rt = app.runtime();
    // PeX on for the session (DHT/LSD stay off: no network in tests).
    let mut p = app.preferences();
    p.bittorrent.pex = true;
    app.set_preferences(p).unwrap();
    let add = |bytes: Vec<u8>, save: &Path| {
        rt.block_on(app.add_torrent(
            TorrentSource::Bytes { data: bytes },
            AddTorrentParams { save_path: Some(save.to_string_lossy().into()), ..Default::default() },
        ))
        .unwrap()
    };
    let a = add(make_torrent(&public.join("p.bin")), &public);
    let b = add(make_private(&make_torrent(&private.join("q.bin"))), &private);
    wait_for(&app, &a, "public seeding", seeding);
    wait_for(&app, &b, "private seeding", seeding);
    let pex_row = |h: &str| app.trackers(h.into()).unwrap().into_iter().find(|t| t.url == "** [PeX] **").unwrap();
    assert_eq!(pex_row(&a).status, TrackerStatus::Working);
    assert_eq!(pex_row(&b).status, TrackerStatus::Disabled);
    assert_eq!(pex_row(&b).message, "This torrent is private");

    let both = vec![a.clone(), b.clone()];
    app.set_pex_disabled(both.clone(), true).unwrap();
    app.set_dht_disabled(both.clone(), true).unwrap();
    let opts = app.discovery_options(both.clone());
    assert_eq!(opts.len(), 2);
    let (pa, pb) = (&opts[0], &opts[1]);
    assert_eq!(
        (pa.hash.as_str(), pa.private, pa.disable_dht, pa.disable_pex, pa.disable_lsd),
        (a.as_str(), false, true, true, false)
    );
    // Private torrents keep their (meaningless) switches untouched.
    assert_eq!((pb.private, pb.disable_dht, pb.disable_pex), (true, false, false));
    assert_eq!(pex_row(&a).status, TrackerStatus::Disabled);
    assert_eq!(pex_row(&a).message, "");

    app.set_dht_disabled(vec![a.clone()], false).unwrap();
    app.shutdown();
    let r = read_resume(&profile, &a);
    assert_eq!((r["disable_dht"].as_bool(), r["disable_pex"].as_bool()), (Some(false), Some(true)));

    let app = start(&profile);
    let opts = app.discovery_options(vec![a.clone()]);
    assert_eq!((opts[0].disable_dht, opts[0].disable_pex, opts[0].disable_lsd), (false, true, false));
    app.shutdown();
}

fn make_hybrid(dir: &Path) -> qbt_core::creator::builder::BuildResult {
    use qbt_core::creator::builder::{BuildParams, build_torrent};
    build_torrent(
        &BuildParams {
            source: dir,
            format: TorrentFormat::Hybrid,
            piece_size: 32 * 1024,
            optimize_alignment: false,
            padding_file_limit: -1,
            private: false,
            trackers: &[],
            url_seeds: &[],
            comment: "",
            source_tag: "",
            created_by: "test",
            creation_date: 0,
            max_torrent_size: 1 << 20,
        },
        &std::sync::atomic::AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// qBittorrent's torrent id rule for hybrid torrents (the truncated v2 hash),
/// the id change of a hybrid added by its v1 hash once its metadata is
/// known, duplicate detection under either hash.
#[test]
fn hybrid_torrent_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = make_data(&downloads, "Hybrid", &[("a.bin", 50_000), ("b.bin", 3_000)]);
    let h = make_hybrid(&data);
    let v1 = hex(&h.info_hash_v1.unwrap());
    let v2 = hex(&h.info_hash_v2.unwrap());
    let id = v2[..40].to_string();
    let app = start(&profile);
    let rt = app.runtime();
    let stopped = || AddTorrentParams { stopped: Some(true), ..Default::default() };
    let conflict = |r: QbtResult<String>| matches!(r, Err(QbtError::Conflict { .. }));

    // Added by its v1 hash: known under it until the metadata arrives.
    let added = rt.block_on(app.add_torrent(TorrentSource::Url { url: format!("magnet:?xt=urn:btih:{v1}") }, stopped())).unwrap();
    assert_eq!(added, v1);
    // A magnet with both hashes is the same torrent.
    let both = format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}");
    assert!(conflict(rt.block_on(app.add_torrent(TorrentSource::Url { url: both.clone() }, stopped()))));
    // The .torrent is a duplicate whose metadata completes the magnet: the
    // torrent takes the v2-based id, its resume data moves along.
    let dup = rt.block_on(app.add_torrent(TorrentSource::Bytes { data: h.data.clone() }, stopped()));
    assert!(conflict(dup));
    assert!(app.torrent(v1.clone()).is_err());
    let t = app.torrent(id.clone()).unwrap();
    assert!(t.has_metadata);
    assert_eq!((t.hash.as_str(), t.infohash_v1.as_str(), t.infohash_v2.as_str()), (id.as_str(), v1.as_str(), v2.as_str()));
    let backup = profile.join("data/BT_backup");
    assert!(backup.join(format!("{id}.torrent")).is_file());
    assert!(backup.join(format!("{id}.fastresume")).is_file());
    assert!(!backup.join(format!("{v1}.fastresume")).exists());
    assert!(!backup.join(format!("{v1}.torrent")).exists());
    // The v1 hash still finds it (qBittorrent's alternative id).
    assert!(conflict(rt.block_on(app.add_torrent(TorrentSource::Url { url: v1.clone() }, stopped()))));
    assert!(conflict(rt.block_on(app.add_torrent(TorrentSource::Url { url: both }, stopped()))));
    assert_eq!(app.torrent_count(), 1);
    rt.block_on(app.remove_torrents(vec![id.clone()], false)).unwrap();

    // Added from the .torrent: the v2-based id at once, and it seeds.
    let added = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: h.data.clone() },
            AddTorrentParams { save_path: Some(downloads.to_string_lossy().into()), ..Default::default() },
        ))
        .unwrap();
    assert_eq!(added, id);
    wait_for(&app, &id, "hybrid seeding", seeding);
    assert!(app.torrent(id.clone()).unwrap().magnet_uri.contains(&format!("xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}")));
    app.shutdown();
    // The engine's fast-resume files (named after its v1 identity) are
    // kept across a restart.
    let fastresume = profile.join("data/engine/fastresume");
    let has_bitv = || std::fs::read_dir(&fastresume).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(&v1));
    assert!(has_bitv());
    let app = start(&profile);
    wait_for(&app, &id, "hybrid seeding after restart", seeding);
    assert!(has_bitv());
    app.shutdown();
}

/// Resume data written under the v1 hash of a hybrid torrent (and of a
/// magnet link with both hashes) moves to the v2-based id on load; every
/// other field is kept.
#[test]
fn hybrid_resume_data_migrates_to_v2_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = make_data(&downloads, "Hybrid", &[("a.bin", 40_000)]);
    let h = make_hybrid(&data);
    let v1 = hex(&h.info_hash_v1.unwrap());
    let v2 = hex(&h.info_hash_v2.unwrap());
    let id = v2[..40].to_string();
    let backup = profile.join("data/BT_backup");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::write(backup.join(format!("{v1}.torrent")), &h.data).unwrap();
    let record = serde_json::json!({
        "version": 1, "info_hash": v1, "category": "Movies", "tags": ["keep"], "save_path": downloads.to_string_lossy(),
        "stopped": true, "added_on": 1_700_000_000, "all_time_upload": 1234, "comment": "mine",
    });
    std::fs::write(backup.join(format!("{v1}.fastresume")), serde_json::to_vec(&record).unwrap()).unwrap();
    // An older both-hash magnet record (its id was the v1 hash then).
    let (mv1, mv2) = ("1".repeat(40), "2".repeat(64));
    let magnet = serde_json::json!({ "version": 1, "info_hash": mv1, "info_hash_v2": mv2, "magnet_name": "M", "stopped": true });
    std::fs::write(backup.join(format!("{mv1}.fastresume")), serde_json::to_vec(&magnet).unwrap()).unwrap();
    // A v1 record stays as it is.
    let plain = "3".repeat(40);
    let v1_record = serde_json::json!({ "version": 1, "info_hash": plain, "stopped": true });
    std::fs::write(backup.join(format!("{plain}.fastresume")), serde_json::to_vec(&v1_record).unwrap()).unwrap();

    let app = start(&profile);
    assert_eq!(app.torrent_count(), 3);
    assert!(app.torrent(v1.clone()).is_err());
    let t = app.torrent(id.clone()).unwrap();
    assert_eq!((t.category.as_str(), t.tags.clone(), t.comment.as_str()), ("Movies", vec!["keep".to_string()], "mine"));
    assert_eq!((t.added_on, t.uploaded), (1_700_000_000, 1234));
    assert_eq!((t.infohash_v1.as_str(), t.infohash_v2.as_str()), (v1.as_str(), v2.as_str()));
    let m = app.torrent(mv2[..40].to_string()).unwrap();
    assert_eq!((m.name.as_str(), m.infohash_v1.as_str(), m.infohash_v2.as_str()), ("M", mv1.as_str(), mv2.as_str()));
    assert!(app.torrent(plain.clone()).is_ok());
    for old in [&v1, &mv1] {
        assert!(!backup.join(format!("{old}.fastresume")).exists(), "{old}");
    }
    assert!(!backup.join(format!("{v1}.torrent")).exists());
    assert!(backup.join(format!("{id}.torrent")).is_file());
    assert_eq!(read_resume(&profile, &id)["category"], "Movies");
    assert_eq!(read_resume(&profile, &mv2[..40])["info_hash_v1"], mv1.as_str());
    app.shutdown();

    // Restarting changes nothing.
    let app = start(&profile);
    assert_eq!(app.torrent_count(), 3);
    assert_eq!(app.torrent(id.clone()).unwrap().category, "Movies");
    app.shutdown();
}

/// Session pause is runtime state: idempotent, announced once per change,
/// never written to a torrent's resume data and gone after a restart.
#[test]
fn session_pause_is_runtime_state() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = make_data(&downloads, "Pausable", &[("a.bin", 90_000)]);
    let torrent = make_torrent(&data);

    let app = start(&profile);
    let rt = app.runtime();
    assert!(!app.is_session_paused());
    let hash = rt.block_on(app.add_torrent(TorrentSource::Bytes { data: torrent }, AddTorrentParams::default())).unwrap();
    wait_for(&app, &hash, "seeding", seeding);

    rt.block_on(app.pause_session()).unwrap();
    rt.block_on(app.pause_session()).unwrap();
    assert!(app.is_session_paused());
    // The torrent keeps its own state (not "stopped"), just without traffic.
    let t = wait_for(&app, &hash, "idle while paused", |t| t.up_speed == 0);
    assert!(!t.state.is_stopped(), "state {:?}", t.state);

    rt.block_on(app.resume_session()).unwrap();
    assert!(!app.is_session_paused());
    wait_for(&app, &hash, "seeding after resume", seeding);

    rt.block_on(app.pause_session()).unwrap();
    app.shutdown();
    drop(app);
    assert_eq!(read_resume(&profile, &hash)["stopped"], serde_json::Value::Bool(false));

    let app2 = start(&profile);
    assert!(!app2.is_session_paused(), "pause must not survive a restart");
    app2.shutdown();
}
