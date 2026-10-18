//! Migration of a synthetic Transmission installation (no real data).

use std::collections::BTreeMap;

use super::*;
use crate::bittorrent::bencode::Value as B;
use crate::migrate::libtorrent::tests::{b, dict, enc};
use crate::migrate::plist::{archive, to_binary, to_xml};

/// A two-file torrent with real piece hashes: "Show/e1.bin" and "Show/e2.txt".
pub(crate) fn torrent(piece_len: usize, e1: &[u8], e2: &[u8], tracker: &str) -> Vec<u8> {
    let mut all = e1.to_vec();
    all.extend_from_slice(e2);
    let mut hashes = Vec::new();
    for chunk in all.chunks(piece_len) {
        use sha1w::ISha1;
        let mut h = sha1w::Sha1::new();
        h.update(chunk);
        hashes.extend_from_slice(&h.finish());
    }
    let info = dict(vec![
        ("name", b("Show")),
        ("piece length", B::Int(piece_len as i64)),
        ("pieces", B::Bytes(hashes)),
        (
            "files",
            B::List(vec![
                dict(vec![("length", B::Int(e1.len() as i64)), ("path", B::List(vec![b("e1.bin")]))]),
                dict(vec![("length", B::Int(e2.len() as i64)), ("path", B::List(vec![b("e2.txt")]))]),
            ]),
        ),
    ]);
    enc(&dict(vec![("announce", b(tracker)), ("info", info)]))
}

fn file_mtime(p: &Path) -> i64 {
    mtime(&std::fs::metadata(p).unwrap())
}

struct Fixture {
    _tmp: tempfile::TempDir,
    opts: Options,
    content: PathBuf,
    /// Complete, proven torrent.
    id: String,
    /// Partial torrent whose file changed after Transmission's check.
    changed_id: String,
    magnet_id: String,
}

fn resume_for(dest: &Path, blocks: B, mtimes: Vec<i64>, extra: Vec<(&str, B)>) -> Vec<u8> {
    let mut entries = vec![
        ("destination", b(&dest.to_string_lossy())),
        ("added-date", B::Int(1_700_000_000)),
        ("done-date", B::Int(1_700_000_500)),
        ("activity-date", B::Int(1_700_000_900)),
        ("downloaded", B::Int(81_000)),
        ("uploaded", B::Int(162_000)),
        ("seeding-time-seconds", B::Int(3600)),
        ("downloading-time-seconds", B::Int(60)),
        ("paused", B::Int(1)),
        ("labels", B::List(vec![b("tv"), b("bad,label")])),
        ("dnd", B::List(vec![B::Int(0), B::Int(0)])),
        ("priority", B::List(vec![B::Int(1), B::Int(-1)])),
        ("speed-limit-down", dict(vec![("speed-Bps", B::Int(50_000)), ("use-speed-limit", B::Int(1)), ("use-global-speed-limit", B::Int(1))])),
        ("speed-limit-up", dict(vec![("speed-Bps", B::Int(9_000)), ("use-speed-limit", B::Int(0)), ("use-global-speed-limit", B::Int(1))])),
        ("ratio-limit", dict(vec![("ratio-limit", b("1.500000")), ("ratio-mode", B::Int(1))])),
        ("idle-limit", dict(vec![("idle-limit", B::Int(45)), ("idle-mode", B::Int(2))])),
        ("files", B::List(vec![b("Show/e1.bin"), b("Show/e2.txt")])),
        ("progress", dict(vec![("blocks", blocks), ("mtimes", B::List(mtimes.into_iter().map(B::Int).collect())), ("pieces", b("none"))])),
        ("peers2", B::List(vec![dict(vec![("flags", B::Int(0)), ("socket_address", B::Bytes(vec![127, 0, 0, 1, 0, 80]))])])),
    ];
    for (k, v) in extra {
        entries.retain(|(x, _)| *x != k);
        entries.push((k, v));
    }
    enc(&dict(entries))
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let src = root.join("Transmission");
    let content = root.join("TV");
    let films = root.join("Films");
    for d in [src.join("Torrents"), src.join("Resume"), src.join("blocklists"), content.join("Show"), films.clone()] {
        std::fs::create_dir_all(d).unwrap();
    }
    // Torrent 1: complete, both files on disk, mtimes recorded.
    let e1: Vec<u8> = (0..80_000u32).map(|i| (i % 251) as u8).collect();
    let e2 = vec![b'x'; 1_000];
    let tor = torrent(32_768, &e1, &e2, "http://tracker.invalid/announce?passkey=SECRET");
    let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
    let hash = meta.info_hash_v1_hex();
    std::fs::write(content.join("Show/e1.bin"), &e1).unwrap();
    std::fs::write(content.join("Show/e2.txt"), &e2).unwrap();
    let mt = vec![file_mtime(&content.join("Show/e1.bin")), file_mtime(&content.join("Show/e2.txt"))];
    std::fs::write(src.join(format!("Torrents/{hash}.torrent")), &tor).unwrap();
    std::fs::write(src.join(format!("Resume/{hash}.resume")), resume_for(&content, b("all"), mt, vec![])).unwrap();

    // Torrent 2: partial (blocks 0-1 = piece 0), its file rewritten since.
    let f1: Vec<u8> = vec![3u8; 70_000];
    let f2 = vec![4u8; 10];
    let tor2 = torrent(32_768, &f1, &f2, "udp://tracker2.invalid:80");
    let meta2 = metainfo::parse_torrent(&tor2, Limits::default()).unwrap().meta;
    let hash2 = meta2.info_hash_v1_hex();
    let other = root.join("Other");
    std::fs::create_dir_all(other.join("Show")).unwrap();
    std::fs::write(other.join("Show/e1.bin.part"), &f1[..32_768]).unwrap();
    // Legacy 3.x file names: <name>.<hash16>.torrent / .resume.
    std::fs::write(src.join(format!("Torrents/Show.{}.torrent", &hash2[..16])), &tor2).unwrap();
    std::fs::write(
        src.join(format!("Resume/Show.{}.resume", &hash2[..16])),
        resume_for(
            &other,
            B::Bytes(vec![0b1100_0000]),
            vec![12345, 0],
            vec![("done-date", B::Int(0)), ("downloaded", B::Int(32_768)), ("files", B::List(vec![]))],
        ),
    )
    .unwrap();

    // A magnet without metadata.
    let mhash = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
    std::fs::write(src.join(format!("Torrents/{mhash}.magnet")), format!("magnet:?xt=urn:btih:{mhash}&dn=Some+Magnet&tr=udp%3A%2F%2Ft.invalid%3A1")).unwrap();
    std::fs::write(src.join(format!("Resume/{mhash}.resume")), resume_for(&content, b("none"), vec![], vec![("done-date", B::Int(0)), ("files", B::List(vec![]))])).unwrap();
    std::fs::write(src.join("Resume/deadbeef.resume.tmp.XYZ"), b"").unwrap();

    // App state.
    let transfer = |h: &str, active: bool, group: i64, remove: bool| {
        Plist::Dict(BTreeMap::from([
            ("TorrentHash".to_string(), Plist::String(h.into())),
            ("Active".to_string(), Plist::Bool(active)),
            ("WaitToStart".to_string(), Plist::Bool(false)),
            ("GroupValue".to_string(), Plist::Int(group)),
            ("RemoveWhenFinishSeeding".to_string(), Plist::Bool(remove)),
        ]))
    };
    let transfers = Plist::Array(vec![transfer(&hash, true, 4, false), transfer(&hash2, false, 9, true), transfer(mhash, false, -1, false)]);
    std::fs::write(src.join("Transfers.plist"), to_xml(&transfers)).unwrap();
    std::fs::write(src.join("queue.json"), format!(r#"["{hash2}.torrent", "{mhash}.magnet", "{hash}.torrent"]"#)).unwrap();
    std::fs::write(src.join("stats.json"), r#"{"downloaded-bytes": 1000, "files-added": 3, "seconds-active": 10, "session-count": 2, "uploaded-bytes": 2000}"#).unwrap();
    std::fs::write(src.join("dht.dat"), b"d2:id20:aaaaaaaaaaaaaaaaaaaae").unwrap();

    // Settings, with an archived group list.
    let group = |idx: i64, name: &str, loc: Option<&Path>| {
        let mut d = BTreeMap::from([("Index".to_string(), Plist::Int(idx)), ("Name".to_string(), Plist::String(name.into()))]);
        d.insert("UsesCustomDownloadLocation".into(), Plist::Bool(loc.is_some()));
        if let Some(l) = loc {
            d.insert("CustomDownloadLocation".into(), Plist::String(l.to_string_lossy().into_owned()));
        }
        d.insert("UsesAutoGroupRules".into(), Plist::Bool(idx == 5));
        Plist::Dict(d)
    };
    let groups = Plist::Array(vec![group(4, "TV Shows", None), group(5, "Films", Some(&films)), group(6, "Unused", None)]);
    let defaults = Plist::Dict(BTreeMap::from([
        ("DownloadFolder".to_string(), Plist::String(root.join("Downloads").to_string_lossy().into_owned())),
        ("UseIncompleteDownloadFolder".to_string(), Plist::Bool(true)),
        ("IncompleteDownloadFolder".to_string(), Plist::String(root.join("Incomplete").to_string_lossy().into_owned())),
        ("CheckDownload".to_string(), Plist::Bool(true)),
        ("DownloadLimit".to_string(), Plist::Int(500)),
        ("SpeedLimitDownloadLimit".to_string(), Plist::Int(20)),
        ("SpeedLimit".to_string(), Plist::Bool(true)),
        ("SpeedLimitAuto".to_string(), Plist::Bool(false)),
        ("BindPort".to_string(), Plist::Int(51_234)),
        ("NatTraversal".to_string(), Plist::Bool(false)),
        ("EncryptionRequire".to_string(), Plist::Bool(true)),
        ("DHTGlobal".to_string(), Plist::Bool(false)),
        ("UTPGlobal".to_string(), Plist::Bool(false)),
        ("Queue".to_string(), Plist::Bool(true)),
        ("QueueDownloadNumber".to_string(), Plist::Int(2)),
        ("QueueSeed".to_string(), Plist::Bool(true)),
        ("QueueSeedNumber".to_string(), Plist::Int(4)),
        ("RatioCheck".to_string(), Plist::Bool(true)),
        ("RatioLimit".to_string(), Plist::Real(1.25)),
        ("IdleLimitCheck".to_string(), Plist::Bool(true)),
        ("IdleLimitMinutes".to_string(), Plist::Int(90)),
        ("PeersTotal".to_string(), Plist::Int(300)),
        ("AutoImport".to_string(), Plist::Bool(true)),
        ("AutoImportDirectory".to_string(), Plist::String(root.join("Watch").to_string_lossy().into_owned())),
        ("BlocklistNew".to_string(), Plist::Bool(true)),
        ("BlocklistURL".to_string(), Plist::String("https://lists.invalid/x?token=SECRET".into())),
        ("RPC".to_string(), Plist::Bool(true)),
        ("RPCAuthorize".to_string(), Plist::Bool(true)),
        ("RPCUsername".to_string(), Plist::String("someone".into())),
        ("CheckQuit".to_string(), Plist::Bool(false)),
        ("DownloadAsk".to_string(), Plist::Bool(false)),
        ("GroupDicts".to_string(), Plist::Data(archive(&groups))),
    ]));
    std::fs::write(root.join("defaults.plist"), to_binary(&defaults)).unwrap();

    let mut opts = Options::macos_defaults(root, root.join("profile"));
    opts.source_dir = src;
    opts.defaults_plist = Some(root.join("defaults.plist"));
    opts.read_system_defaults = false;
    Fixture { _tmp: tmp, opts, content, id: meta.id(), changed_id: meta2.id(), magnet_id: mhash.into() }
}

fn record(profile: &Path, id: &str) -> FastResume {
    serde_json::from_slice(&std::fs::read(profile.join(format!("data/BT_backup/{id}.fastresume"))).unwrap()).unwrap()
}

#[test]
fn decodes_resume_files() {
    let data = enc(&dict(vec![
        ("destination", b("/d")),
        ("added_date", B::Int(5)),
        ("progress", dict(vec![("blocks", B::Bytes(vec![0xff, 0x80])), ("time-checked", B::List(vec![B::Int(9), B::List(vec![B::Int(100), B::Int(3), B::Int(1)])]))])),
        ("ratio-limit", dict(vec![("ratio-limit", b("0.500000")), ("ratio-mode", B::Int(1))])),
        ("speed-limit-up", dict(vec![("speed", B::Int(10)), ("use-speed-limit", B::Int(1))])),
    ]));
    let r = TrResume::parse(&data, Limits::default()).unwrap();
    assert_eq!(r.added_date, 5, "snake_case keys of TR_SAVE_VERSION_FORMAT=5");
    assert_eq!(r.time_checked, Some(vec![9, 101]));
    assert_eq!((r.ratio_mode, r.ratio_limit), (1, 0.5));
    assert_eq!(r.upload_limit, Some(10 * 1024), "old KiB/s value");
    assert!(TrResume::parse(b"d4:infoi1ee", Limits::default()).is_err(), "no destination");
    assert!(TrResume::parse(b"li1ee", Limits::default()).is_err());

    // 9 blocks of 16 KiB, pieces of 2 blocks: pieces 0-3 complete, piece 4 (one block) too.
    let tor = crate::bittorrent::metainfo::tests::make_torrent("x", &[(&[], 9 * BLOCK_SIZE)], 2 * BLOCK_SIZE, &[]);
    let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
    assert_eq!(r.have_pieces(&meta).unwrap(), vec![true, true, true, true, true]);
    let mut r2 = r.clone();
    r2.blocks = Blocks::Raw(vec![0b1011_1111]);
    assert_eq!(r2.have_pieces(&meta).unwrap(), vec![false, true, true, true, false], "short bitfields mean missing blocks");
    r2.blocks = Blocks::Raw(vec![0; 3]);
    assert!(r2.have_pieces(&meta).is_err(), "longer than the torrent");
    r2.blocks = Blocks::Missing;
    assert!(r2.have_pieces(&meta).is_err());
    r2.blocks = Blocks::None;
    assert_eq!(r2.have_pieces(&meta).unwrap(), vec![false; 5]);
}

#[test]
fn reads_app_files() {
    let (map, order) = parse_transfers(
        to_xml(&Plist::Array(vec![Plist::Dict(BTreeMap::from([
            ("TorrentHash".to_string(), Plist::String("ABCDEF".into())),
            ("Active".to_string(), Plist::Bool(true)),
            ("GroupValue".to_string(), Plist::Int(2)),
        ]))]))
        .as_bytes(),
    )
    .unwrap();
    assert_eq!(order, vec!["abcdef"]);
    assert_eq!(map["abcdef"], Transfer { active: true, wait_to_start: false, group: 2, remove_when_finished_seeding: false });
    assert!(parse_transfers(b"<plist><dict/></plist>").is_err());

    let defaults = Plist::Dict(BTreeMap::new());
    let groups = parse_groups(&defaults).unwrap();
    assert_eq!(groups.len(), 7, "Transmission's color groups when none are saved");
    assert_eq!(groups[6].name, "Gray");

    let h = "c12fe1c06bba254a9dc9f519b335aa7c1367a88a";
    assert_eq!(parse_queue(format!(r#"["{h}.torrent", "nonsense", "{}.magnet"]"#, h.to_uppercase()).as_bytes()), vec![h, h]);
    assert_eq!(parse_stats(br#"{"downloaded-bytes": 5, "uploaded-bytes": -1}"#), Some((5, 0)));
}

#[test]
fn maps_settings() {
    let f = fixture();
    let d = plist::parse(&std::fs::read(f.opts.defaults_plist.as_ref().unwrap()).unwrap()).unwrap();
    let m = map_settings(&d);
    let p = &m.prefs;
    assert!(p.downloads.save_path.ends_with("/Downloads"));
    assert!(p.downloads.temp_path_enabled && p.downloads.temp_path.ends_with("/Incomplete"));
    assert_eq!((p.speed.dl_limit, p.speed.up_limit, p.speed.alt_dl_limit), (500_000, 0, 20_000));
    assert!(m.alt_speed);
    assert_eq!(p.connection.listen_port, 51_234);
    assert!(!p.connection.upnp);
    assert_eq!(p.connection.max_connec, 300);
    assert_eq!(p.bittorrent.encryption, EncryptionMode::Require);
    assert!(!p.bittorrent.dht && p.bittorrent.pex);
    assert_eq!(p.connection.bittorrent_protocol, BtProtocol::Tcp);
    assert!(p.bittorrent.queueing_enabled);
    assert_eq!((p.bittorrent.max_active_downloads, p.bittorrent.max_active_uploads, p.bittorrent.max_active_torrents), (2, 4, 6));
    assert_eq!((p.bittorrent.max_ratio, p.bittorrent.max_inactive_seeding_time), (1.25, 90));
    assert_eq!(p.downloads.watched_folders.len(), 1);
    assert!(!p.webui.web_ui_enabled, "Transmission's RPC never turns the Web UI on");
    let unmapped = format!("{:?}", m.unmapped);
    assert!(unmapped.contains("RPC") && unmapped.contains("blocklist"));
    assert!(!unmapped.contains("SECRET") && !unmapped.contains("someone"));
    assert!(m.gui.contains(&("Preferences/General/ExitConfirm".to_owned(), GuiValue::Bool(false))));

    // Schedule days.
    let mut d2 = d.as_dict().unwrap().clone();
    d2.insert("SpeedLimitAuto".into(), Plist::Bool(true));
    d2.insert("SpeedLimitAutoDay".into(), Plist::Int(62));
    let m2 = map_settings(&Plist::Dict(d2.clone()));
    assert!(m2.prefs.speed.scheduler_enabled && !m2.alt_speed);
    assert_eq!(m2.prefs.speed.scheduler_days, SchedulerDays::Weekday);
    d2.insert("SpeedLimitAutoDay".into(), Plist::Int(3));
    assert_eq!(map_settings(&Plist::Dict(d2)).prefs.speed.scheduler_days, SchedulerDays::EveryDay);
}

#[test]
fn dry_run_writes_nothing() {
    let f = fixture();
    let mut opts = f.opts.clone();
    opts.dry_run = true;
    let rep = run(&opts).unwrap();
    assert!(!opts.profile.exists());
    assert_eq!(rep.torrents.len(), 3, "{:?}", rep.failed_torrents);
    let text = rep.to_text();
    assert!(text.starts_with("Transmission -> qBit NeoRS migration (dry run"));
    assert!(!text.contains("SECRET"), "tracker passkeys and URLs never appear");
    assert!(!text.contains("someone"));
    assert!(text.contains("remote access"));
    assert!(rep.remote_access_enabled);
    assert!(!text.contains("Search plugins"));
}

#[test]
fn migrates_everything() {
    let f = fixture();
    let rep = run(&f.opts).unwrap();
    let p = &f.opts.profile;
    assert!(rep.failed_torrents.is_empty(), "{:?}", rep.failed_torrents);
    assert_eq!(rep.torrents.len(), 3);

    let prefs: Preferences = serde_json::from_slice(&std::fs::read(p.join("config/preferences.json")).unwrap()).unwrap();
    assert_eq!(prefs.connection.listen_port, 51_234);
    let cats = categories::from_json(&std::fs::read(p.join("config/categories.json")).unwrap()).unwrap();
    assert_eq!(cats.keys().collect::<Vec<_>>(), vec!["Films", "TV Shows"], "used groups and groups with a folder");
    assert!(cats["Films"].save_path.ends_with("/Films"));
    let tags: Vec<String> = serde_json::from_slice(&std::fs::read(p.join("config/tags.json")).unwrap()).unwrap();
    assert_eq!(tags, vec!["tv"]);
    let stats: StatsFile = resume::load_json(&p.join("data/stats.json"));
    assert_eq!((stats.alltime_dl, stats.alltime_ul), (1000, 2000));
    let state: SessionStateFile = resume::load_json(&p.join("data/session_state.json"));
    assert!(state.use_alt_speed_limits);
    assert!(std::fs::read_to_string(p.join("config/gui-preferences.plist")).unwrap().contains("AddNewTorrentDialog/Enabled"));
    assert!(p.join("migration/report.txt").is_file());

    // The complete torrent: everything carried over, pieces imported.
    let r = record(p, &f.id);
    assert!(!r.stopped, "Active in Transfers.plist");
    assert_eq!(r.category, "TV Shows");
    assert_eq!(r.tags, vec!["tv"]);
    assert!(!r.auto_tmm);
    assert_eq!(r.save_path, f.content.to_string_lossy());
    assert_eq!(r.location, f.content.to_string_lossy());
    assert_eq!(r.file_paths, vec!["Show/e1.bin", "Show/e2.txt"]);
    assert_eq!(r.file_priorities, vec![6, 1]);
    assert_eq!((r.download_limit, r.upload_limit), (50_000, 0));
    assert_eq!((r.ratio_limit, r.inactive_seeding_time_limit), (1.5, -1));
    assert_eq!((r.added_on, r.completed_on, r.last_upload), (1_700_000_000, 1_700_000_500, 1_700_000_900));
    assert_eq!((r.all_time_download, r.all_time_upload, r.seeding_time, r.active_time), (81_000, 162_000, 3600, 3660));
    assert!(r.seed_status);
    assert_eq!(r.queue_position, -1, "complete torrents are not queued");
    assert_eq!(r.trackers.len(), 1);
    assert_eq!(r.completed_bytes, 81_000);
    let meta = metainfo::parse_torrent(&std::fs::read(p.join(format!("data/BT_backup/{}.torrent", f.id))).unwrap(), Limits::default()).unwrap().meta;
    assert_eq!(std::fs::read(p.join(format!("data/engine/fastresume/{}.bitv", meta.engine_hash_hex()))).unwrap(), vec![0b1110_0000]);
    let sum = rep.torrents.iter().find(|t| t.id == f.id).unwrap();
    assert_eq!(sum.progress, Some(1.0));
    assert!(sum.notes.iter().any(|n| n.contains("bad,label")));
    assert!(sum.notes.iter().any(|n| n.contains("low-priority")));

    // The changed torrent: rechecked, .part name kept, queue position, Remove.
    let r2 = record(p, &f.changed_id);
    assert!(r2.stopped);
    assert_eq!(r2.category, "", "group 9 does not exist");
    assert_eq!(r2.file_paths[0], "Show/e1.bin.part");
    assert!(r2.location.ends_with("/Other"));
    assert_eq!(r2.queue_position, 0);
    assert_eq!(r2.share_limit_action, "Remove");
    assert!(r2.download_path.is_empty() && !r2.seed_status);
    assert!(rep.torrents.iter().find(|t| t.id == f.changed_id).unwrap().pieces.starts_with("full recheck"));
    let meta2 = metainfo::parse_torrent(&std::fs::read(p.join(format!("data/BT_backup/{}.torrent", f.changed_id))).unwrap(), Limits::default()).unwrap().meta;
    assert!(!p.join(format!("data/engine/fastresume/{}.bitv", meta2.engine_hash_hex())).exists());

    // The magnet.
    let m = record(p, &f.magnet_id);
    assert_eq!(m.magnet_name, "Some Magnet");
    assert_eq!(m.queue_position, 1);
    assert!(!p.join(format!("data/BT_backup/{}.torrent", f.magnet_id)).exists());

    // Transmission's folder is unchanged; a second run refuses the profile.
    assert!(f.opts.source_dir.join("Transfers.plist").is_file());
    assert!(run(&f.opts).unwrap_err().contains("already holds"));
}

#[test]
fn merges_once() {
    let f = fixture();
    run(&f.opts).unwrap();
    let p = &f.opts.profile;
    // Torrents the profile has keep their state; statistics are added once.
    let mut o = f.opts.clone();
    o.merge = true;
    let rep = run(&o).unwrap();
    assert!(rep.torrents.iter().all(|t| t.state.starts_with("merged")));
    let r = record(p, &f.id);
    assert_eq!(r.all_time_upload, 2 * 162_000);
    let stats: StatsFile = resume::load_json(&p.join("data/stats.json"));
    assert_eq!(stats.alltime_ul, 4000);
    run(&o).unwrap();
    assert_eq!(record(p, &f.id).all_time_upload, 2 * 162_000, "never counted twice");
    assert_eq!(resume::load_json::<StatsFile>(&p.join("data/stats.json")).alltime_ul, 4000);
    assert!(p.join("migration/gui-preferences.plist").is_file(), "merge only offers the GUI settings");
}

#[test]
fn backs_up_the_profile() {
    let f = fixture();
    assert_eq!(super::super::backup_profile(&f.opts.profile).unwrap(), None);
    run(&f.opts).unwrap();
    let dest = super::super::backup_profile(&f.opts.profile).unwrap().unwrap();
    assert!(dest.file_name().unwrap().to_string_lossy().starts_with("profile.before-migration-"));
    assert!(dest.join("config/preferences.json").is_file());
    assert!(dest.join(format!("data/BT_backup/{}.fastresume", f.id)).is_file());
    assert!(dest.join("data/engine/fastresume").is_dir());
    let again = super::super::backup_profile(&f.opts.profile).unwrap().unwrap();
    assert_ne!(again, dest, "never reuses a backup folder");
}
