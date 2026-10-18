//! End-to-end migration of a synthetic qBittorrent installation.

use super::*;
use crate::bittorrent::bencode::Value as B;
use crate::migrate::libtorrent::tests::{b, dict, enc, sample_torrent};

struct Fixture {
    _tmp: tempfile::TempDir,
    opts: Options,
    content: PathBuf,
    id: String,
    password_hash: String,
}

/// A qBittorrent layout with one torrent: "a.mkv" (40 bytes, wanted,
/// complete) and "b.nfo" (4 bytes, skipped, its bytes in the part file).
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let (config, prefs_dir, data, content) = (root.join("config"), root.join("prefs"), root.join("data"), root.join("Movies"));
    for d in [&config, &prefs_dir.join("rss"), &data.join("BT_backup"), &data.join("nova3/engines/__pycache__"), &data.join("SearchUI"), &content] {
        std::fs::create_dir_all(d).unwrap();
    }
    let password_hash = crate::webui::password::hash_password("correct horse");
    std::fs::write(
        config.join("qBittorrent.ini"),
        format!(
            concat!(
                "[BitTorrent]\nSession\\Port=52739\nSession\\Tags=Movie, \u{77e2}\u{6ca2}\nSession\\AddExtensionToIncompleteFiles=true\n",
                "Session\\DefaultSavePath={}\nSession\\QueueingSystemEnabled=false\n",
                "[Application]\nFileLogger\\Path={}/logs\n",
                "[GUI]\nStartUpWindowState=Minimized\n",
                "[Preferences]\nWebUI\\Enabled=true\nWebUI\\Port=9292\nWebUI\\Username=admin\nWebUI\\LocalHostAuth=false\n",
                "WebUI\\Password_PBKDF2=\"@ByteArray({})\"\nGeneral\\ExitConfirm=false\n",
                "[SearchEngines]\ndisabledEngines=piratebay, limetorrents\n",
                "[MainWindow]\ngeometry=@ByteArray(\\x1\\xd9)\n",
            ),
            root.display(),
            data.display(),
            password_hash
        ),
    )
    .unwrap();
    std::fs::write(
        config.join("qBittorrent-data.ini"),
        "[Stats]\nAllStats=@Variant(\\0\\0\\0\\x1c\\0\\0\\0\\x1\\0\\0\\0\\x12\\0\\x41\\0l\\0l\\0t\\0i\\0m\\0\\x65\\0U\\0L\\0\\0\\0\\x4\\0\\0\\0\\0\\0\\0\\0\\x7b)\n",
    )
    .unwrap();
    std::fs::write(prefs_dir.join("categories.json"), format!(r#"{{"Movies": {{"save_path": "{}"}}, "Music": {{"save_path": "/m", "download_path": ""}}}}"#, content.display()))
        .unwrap();
    std::fs::write(
        prefs_dir.join("watched_folders.json"),
        r#"{"/w": {"recursive": false, "add_torrent_params": {"category": "", "use_auto_tmm": true, "tags": []}}}"#,
    )
    .unwrap();
    std::fs::write(prefs_dir.join("rss/feeds.json"), r#"{"News": {"uid": "{6d1c1c3c-0000-4000-8000-000000000001}", "url": "http://example.invalid/rss"}}"#).unwrap();
    std::fs::write(prefs_dir.join("rss/download_rules.json"), r#"{"r1": {"enabled": true, "mustContain": "x"}}"#).unwrap();
    std::fs::write(data.join("nova3/engines/eng1.py"), "# engine").unwrap();
    std::fs::write(data.join("nova3/engines/eng1.png"), [137u8, 80]).unwrap();
    std::fs::write(data.join("nova3/engines/__init__.py"), "").unwrap();
    std::fs::write(data.join("nova3/engines/__pycache__/eng1.pyc"), "x").unwrap();
    std::fs::write(data.join("SearchUI/History.txt"), "ubuntu\ndebian\n").unwrap();

    let a: Vec<u8> = (0..40u8).collect();
    let tor = sample_torrent(16, &a, b"nfo!");
    let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
    let id = meta.id();
    std::fs::write(content.join("a.mkv"), &a).unwrap();
    // libtorrent part file: piece 2 (a's last 8 bytes + b.nfo) in slot 0.
    let mut pf = Vec::new();
    pf.extend_from_slice(&3u32.to_be_bytes());
    pf.extend_from_slice(&16u32.to_be_bytes());
    for s in [u32::MAX, u32::MAX, 0] {
        pf.extend_from_slice(&s.to_be_bytes());
    }
    pf.resize(1024, 0);
    let mut slot = vec![0u8; 16];
    slot[8..12].copy_from_slice(b"nfo!");
    pf.extend_from_slice(&slot);
    std::fs::write(content.join(format!(".{id}.parts")), pf).unwrap();

    let resume = enc(&dict(vec![
        ("info-hash", B::Bytes(meta.info_hash.to_vec())),
        ("pieces", B::Bytes(vec![1, 1, 1])),
        ("file_priority", B::List(vec![B::Int(1), B::Int(0)])),
        ("mapped_files", B::List(vec![b("a.mkv"), b("b.nfo")])),
        ("trackers", B::List(vec![B::List(vec![b("http://tracker.invalid/announce?passkey=SECRET")])])),
        ("save_path", b(&content.to_string_lossy())),
        ("paused", B::Int(0)),
        ("auto_managed", B::Int(1)),
        ("added_time", B::Int(1_700_000_000)),
        ("completed_time", B::Int(1_700_000_100)),
        ("seeding_time", B::Int(77)),
        ("total_uploaded", B::Int(4000)),
        ("total_downloaded", B::Int(44)),
        ("qBt-category", b("Movies")),
        ("qBt-tags", B::List(vec![b("Movie")])),
        ("qBt-savePath", b("")),
        ("qBt-ratioLimit", B::Int(-2000)),
        ("qBt-contentLayout", b("NoSubfolder")),
        ("qBt-seedStatus", B::Int(1)),
    ]));
    std::fs::write(data.join(format!("BT_backup/{id}.fastresume")), resume).unwrap();
    std::fs::write(data.join(format!("BT_backup/{id}.torrent")), &tor).unwrap();
    std::fs::write(data.join("BT_backup/queue"), format!("{id}\n")).unwrap();

    let mut opts = Options::macos_defaults(root, root.join("profile"));
    opts.source_config = config;
    opts.source_prefs = prefs_dir;
    opts.source_data = data;
    Fixture { _tmp: tmp, opts, content, id, password_hash }
}

#[test]
fn dry_run_writes_nothing() {
    let f = fixture();
    let mut opts = f.opts.clone();
    opts.dry_run = true;
    let rep = run(&opts).unwrap();
    assert!(!opts.profile.exists());
    assert!(!f.content.join("b.nfo").exists(), "dry run never creates skipped files");
    assert_eq!(rep.torrents.len(), 1);
    assert_eq!(rep.torrents[0].progress, Some(1.0));
    assert_eq!(rep.skipped_files_created, vec![f.content.join("b.nfo").display().to_string()]);
    let text = rep.to_text();
    assert!(text.contains("dry run"));
    assert!(!text.contains("SECRET"), "tracker passkeys never appear");
    assert!(!text.contains(&f.password_hash), "password hash never appears");
    assert!(text.contains("geometry"), "unmapped ini keys are listed");
}

#[test]
fn migrates_everything() {
    let f = fixture();
    let rep = run(&f.opts).unwrap();
    let p = &f.opts.profile;
    assert!(rep.failed_torrents.is_empty(), "{:?}", rep.failed_torrents);

    // Preferences.
    let prefs: Preferences = serde_json::from_slice(&std::fs::read(p.join("config/preferences.json")).unwrap()).unwrap();
    assert_eq!(prefs.connection.listen_port, 52739);
    assert_eq!((prefs.webui.web_ui_port, prefs.webui.web_ui_username.as_str()), (9292, "admin"));
    assert!(prefs.webui.web_ui_enabled && prefs.webui.bypass_local_auth);
    assert!(crate::webui::password::verify_password(&prefs.webui.web_ui_password_pbkdf2, "correct horse"));
    assert_eq!(prefs.behavior.file_log_path, "");
    assert_eq!(prefs.downloads.watched_folders.len(), 1);

    // Categories, tags, search, RSS, stats, GUI.
    let cats = categories::from_json(&std::fs::read(p.join("config/categories.json")).unwrap()).unwrap();
    assert_eq!(cats.keys().cloned().collect::<Vec<_>>(), vec!["Movies", "Music"]);
    let tags: Vec<String> = serde_json::from_slice(&std::fs::read(p.join("config/tags.json")).unwrap()).unwrap();
    assert_eq!(tags, vec!["Movie", "\u{77e2}\u{6ca2}"]);
    assert!(p.join("data/nova3/engines/eng1.py").is_file() && p.join("data/nova3/engines/eng1.png").is_file());
    assert!(!p.join("data/nova3/engines/__init__.py").exists());
    let search: Value = serde_json::from_slice(&std::fs::read(p.join("config/search.json")).unwrap()).unwrap();
    assert_eq!(search["disabled_engines"], serde_json::json!(["piratebay", "limetorrents"]));
    assert_eq!((rep.rss_feeds, rep.rss_rules), (1, 1));
    assert!(p.join("data/rss/feeds.json").is_file() && p.join("data/rss/download_rules.json").is_file());
    let stats: StatsFile = serde_json::from_slice(&std::fs::read(p.join("data/stats.json")).unwrap()).unwrap();
    assert_eq!(stats.alltime_ul, 123);
    let plist = std::fs::read_to_string(p.join("config/gui-preferences.plist")).unwrap();
    assert!(plist.contains("<key>GUI/StartUpWindowState</key>\n\t<integer>1</integer>"));
    assert!(plist.contains("<string>debian</string>"));
    let keys = std::fs::read_to_string(p.join("migration/gui-preferences.keys")).unwrap();
    assert!(keys.lines().any(|k| k == "Preferences/General/ExitConfirm"));
    let report = std::fs::read_to_string(p.join("migration/report.txt")).unwrap();
    assert!(!report.contains("SECRET") && !report.contains(&f.password_hash));

    // The torrent: record, metadata, bitfield, recovered skipped file.
    let (records, failed) = BackupDir::new(p.join("data/BT_backup")).load_all();
    assert!(failed.is_empty());
    let r = &records[0];
    assert_eq!(r.info_hash, f.id);
    assert!(r.auto_tmm && r.category == "Movies" && r.tags == vec!["Movie"]);
    assert_eq!(r.file_paths, vec!["a.mkv", "b.nfo"]);
    assert_eq!(r.file_priorities, vec![1, 0]);
    assert_eq!(r.location, f.content.to_string_lossy());
    assert_eq!((r.all_time_upload, r.all_time_download, r.seeding_time), (4000, 44, 77));
    assert!(!r.stopped && r.seed_status);
    assert_eq!(r.trackers.len(), 1);
    assert_eq!(r.completed_bytes, 40);
    assert!(p.join(format!("data/BT_backup/{}.torrent", f.id)).is_file());
    let bitv = std::fs::read(p.join(format!("data/engine/fastresume/{}.bitv", f.id))).unwrap();
    assert_eq!(bitv, vec![0b1110_0000]);
    assert_eq!(std::fs::read(f.content.join("b.nfo")).unwrap(), b"nfo!");

    // A second run refuses to overwrite the profile.
    assert!(run(&f.opts).unwrap_err().contains("already holds"));
}

#[test]
fn sandbox_and_stop_all() {
    let f = fixture();
    let mut opts = f.opts.clone();
    opts.stop_all = true;
    opts.sandbox = Some(Sandbox { listen_port: 40001, webui_port: 40002 });
    opts.skipped_data = SkippedData::Unclaim;
    let rep = run(&opts).unwrap();
    let p = &opts.profile;
    let prefs: Preferences = serde_json::from_slice(&std::fs::read(p.join("config/preferences.json")).unwrap()).unwrap();
    assert_eq!((prefs.connection.listen_port, prefs.webui.web_ui_port), (40001, 40002));
    assert!(!prefs.bittorrent.dht && !prefs.bittorrent.lsd && !prefs.bittorrent.pex && !prefs.connection.upnp);
    assert!(prefs.downloads.watched_folders.is_empty());
    let (records, _) = BackupDir::new(p.join("data/BT_backup")).load_all();
    assert!(records[0].stopped);
    // Unclaimed: the shared last piece is dropped, nothing is written.
    assert!(!f.content.join("b.nfo").exists());
    assert_eq!(std::fs::read(p.join(format!("data/engine/fastresume/{}.bitv", f.id))).unwrap(), vec![0b1100_0000]);
    assert_eq!(rep.torrents[0].progress, Some(32.0 / 40.0));
}

#[test]
fn merge_into_existing_profile() {
    let f = fixture();
    let mut opts = f.opts.clone();
    opts.merge = true;
    assert!(run(&opts).unwrap_err().contains("no qBit NeoRS profile"));

    // A profile where the torrent was added by hand: fresh statistics, a
    // flat tracker list plus a tracker of its own, its own settings.
    run(&f.opts).unwrap();
    let p = &f.opts.profile;
    let backup = BackupDir::new(p.join("data/BT_backup"));
    let (mut records, _) = backup.load_all();
    let mut r = records.remove(0);
    r.all_time_upload = 10;
    r.all_time_download = 1;
    r.seeding_time = 5;
    r.added_on = 1_800_000_000;
    r.trackers = vec![crate::model::TrackerTierEntry { url: "udp://mine:1".into(), tier: 0 }];
    r.queue_position = -1;
    r.category = "Other".into();
    backup.save_resume(&r).unwrap();
    std::fs::write(p.join("data/stats.json"), r#"{"alltime_dl": 1, "alltime_ul": 7}"#).unwrap();
    let mut prefs: Preferences = serde_json::from_slice(&std::fs::read(p.join("config/preferences.json")).unwrap()).unwrap();
    prefs.webui.web_ui_port = 1234;
    std::fs::write(p.join("config/preferences.json"), serde_json::to_vec(&prefs).unwrap()).unwrap();
    let bitv = std::fs::read(p.join(format!("data/engine/fastresume/{}.bitv", f.id))).unwrap();

    let rep = run(&opts).unwrap();
    assert!(rep.failed_torrents.is_empty());
    let check = || {
        let (records, _) = backup.load_all();
        let r = &records[0];
        assert_eq!((r.all_time_upload, r.all_time_download, r.seeding_time), (4010, 45, 82));
        assert_eq!(r.added_on, 1_700_000_000);
        assert_eq!(r.trackers.len(), 2);
        assert_eq!((r.trackers[0].tier, r.trackers[1].url.as_str(), r.trackers[1].tier), (0, "udp://mine:1", 1));
        assert_eq!(r.queue_position, 0);
        assert_eq!(r.category, "Other", "the profile's choices are kept");
        let stats: StatsFile = serde_json::from_slice(&std::fs::read(p.join("data/stats.json")).unwrap()).unwrap();
        assert_eq!((stats.alltime_dl, stats.alltime_ul), (1, 130));
    };
    check();
    assert!(rep.torrents[0].notes.iter().any(|n| n.contains("category differs")));
    let prefs: Preferences = serde_json::from_slice(&std::fs::read(p.join("config/preferences.json")).unwrap()).unwrap();
    assert_eq!(prefs.webui.web_ui_port, 1234, "settings are kept");
    assert_eq!(std::fs::read(p.join(format!("data/engine/fastresume/{}.bitv", f.id))).unwrap(), bitv, "no recheck");
    assert!(p.join("migration/gui-preferences.plist").is_file());

    // Running it again changes nothing.
    run(&opts).unwrap();
    check();
}

#[test]
fn refuses_profile_inside_source() {
    let f = fixture();
    let mut opts = f.opts.clone();
    opts.profile = opts.source_data.join("neo");
    assert!(run(&opts).unwrap_err().contains("separate"));
}
