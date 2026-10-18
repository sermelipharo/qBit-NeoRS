//! Import of a synthetic Transmission profile (the app's in-app import
//! path: backup + migration), then a session started on the result. Local
//! only: no trackers, discovery off, WebUI on 127.0.0.1.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use qbt_core::bittorrent::bencode::{Value, encode};
use qbt_core::bittorrent::metainfo;
use qbt_core::migrate::import::{ImportSource, run_in};
use qbt_core::migrate::{Sandbox, apply_sandbox};
use qbt_core::model::TorrentState;
use qbt_core::prefs::Preferences;
use qbt_core::{App, AppConfig};
use qbt_e2e::*;

fn d(entries: Vec<(&str, Value)>) -> Value {
    Value::Dict(entries.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect::<BTreeMap<_, _>>())
}

fn s(v: &str) -> Value {
    Value::Bytes(v.as_bytes().to_vec())
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p).unwrap().modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// A `.resume` file: data in `dest`, `blocks` claimed, file mtimes recorded.
fn resume(dest: &Path, blocks: Value, mtimes: Vec<i64>, done: bool) -> Vec<u8> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut out = Vec::new();
    encode(
        &d(vec![
            ("destination", s(&dest.to_string_lossy())),
            ("added-date", Value::Int(now - 1000)),
            ("done-date", Value::Int(if done { now - 500 } else { 0 })),
            ("uploaded", Value::Int(12_345)),
            ("downloaded", Value::Int(400_000)),
            ("paused", Value::Int(0)),
            ("labels", Value::List(vec![s("imported")])),
            ("progress", d(vec![("blocks", blocks), ("mtimes", Value::List(mtimes.into_iter().map(Value::Int).collect())), ("pieces", s("none"))])),
        ]),
        &mut out,
    );
    out
}

#[test]
fn imports_a_transmission_profile_and_starts_it() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let data = tmp.path().join("data");
    let tr = home.join("Library/Application Support/Transmission");
    for dir in [tr.join("Torrents"), tr.join("Resume"), home.join("Library/Preferences"), data.clone()] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // A complete torrent and one with only its first piece.
    let full = make_content(&data, "Complete Sample", &[("a.bin", 300_000), ("b.bin", 100_000)]);
    let full_torrent = make_torrent(&full, 64 * 1024);
    let full_meta = metainfo::parse_torrent(&full_torrent, Default::default()).unwrap().meta;
    let half = make_content(&data, "Partial Sample", &[("c.bin", 200_000)]);
    let half_torrent = make_torrent(&half, 64 * 1024);
    let half_meta = metainfo::parse_torrent(&half_torrent, Default::default()).unwrap().meta;
    let (fh, hh) = (full_meta.info_hash_v1_hex(), half_meta.info_hash_v1_hex());
    std::fs::write(tr.join(format!("Torrents/{fh}.torrent")), &full_torrent).unwrap();
    std::fs::write(tr.join(format!("Torrents/{hh}.torrent")), &half_torrent).unwrap();
    let file_times = |m: &metainfo::TorrentMeta| m.files.iter().map(|f| mtime(&data.join(&f.path))).collect::<Vec<_>>();
    std::fs::write(tr.join(format!("Resume/{fh}.resume")), resume(&data, s("all"), file_times(&full_meta), true)).unwrap();
    // 16 KiB blocks: the first four make piece 0.
    std::fs::write(tr.join(format!("Resume/{hh}.resume")), resume(&data, Value::Bytes(vec![0xf0]), file_times(&half_meta), false)).unwrap();
    std::fs::write(
        tr.join("Transfers.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><array>\
             <dict><key>TorrentHash</key><string>{fh}</string><key>Active</key><true/><key>GroupValue</key><integer>1</integer></dict>\
             <dict><key>TorrentHash</key><string>{hh}</string><key>Active</key><false/><key>GroupValue</key><integer>-1</integer></dict>\
             </array></plist>\n"
        ),
    )
    .unwrap();
    std::fs::write(tr.join("stats.json"), r#"{"downloaded-bytes": 400000, "uploaded-bytes": 12345}"#).unwrap();
    std::fs::write(
        home.join("Library/Preferences/org.m0k.transmission.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>DownloadFolder</key><string>{}</string><key>DHTGlobal</key><false/>\
             <key>RatioCheck</key><true/><key>RatioLimit</key><real>3</real></dict></plist>\n",
            data.display()
        ),
    )
    .unwrap();

    // An existing, empty profile (what the app creates on its first launch).
    let profile = tmp.path().join("profile");
    std::fs::create_dir_all(profile.join("config")).unwrap();
    std::fs::write(profile.join("config/preferences.json"), b"{}").unwrap();

    let outcome = run_in(ImportSource::Transmission, &home, &profile, false).expect("import");
    let plan = &outcome.plan;
    assert!(!plan.merge, "an empty profile takes Transmission's settings");
    assert_eq!((plan.torrents, plan.torrents_with_pieces, plan.torrents_to_check, plan.torrents_failed), (2, 2, 0, 0));
    assert_eq!(plan.categories, 1, "group 1 (Orange) is used");
    assert!(Path::new(&outcome.backup_dir).join("config/preferences.json").is_file(), "the profile was backed up first");
    assert!(Path::new(&outcome.report_path).is_file());
    assert!(tr.join("Transfers.plist").is_file() && tr.join(format!("Resume/{fh}.resume")).is_file(), "Transmission's files untouched");

    // Local-only settings for the test session.
    let prefs_path = profile.join("config/preferences.json");
    let mut prefs: Preferences = serde_json::from_slice(&std::fs::read(&prefs_path).unwrap()).unwrap();
    assert!(!prefs.bittorrent.dht);
    assert_eq!(prefs.bittorrent.max_ratio, 3.0);
    apply_sandbox(&mut prefs, &Sandbox { listen_port: free_port(), webui_port: free_port() });
    prefs.behavior.file_log_enabled = false;
    std::fs::write(&prefs_path, serde_json::to_vec_pretty(&prefs).unwrap()).unwrap();

    let app = App::new(AppConfig { profile_dir: profile.to_string_lossy().into_owned(), front_end: "e2e".into(), ..Default::default() }).expect("App::new");
    let node_wait = |hash: &str, what: &str, pred: &dyn Fn(&qbt_core::model::TorrentInfo) -> bool| {
        let start = std::time::Instant::now();
        loop {
            if let Ok(t) = app.torrent(hash.to_owned())
                && pred(&t)
            {
                return t;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "timed out waiting for {what}: {:?}", app.torrent(hash.to_owned()).ok());
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    let full_id = full_meta.id();
    let t = node_wait(&full_id, "the complete torrent seeding", &|t| is_seeding(t));
    assert_eq!(t.category, "Orange");
    assert_eq!(t.tags, vec!["imported".to_string()]);
    assert_eq!(t.save_path.trim_end_matches('/'), data.to_string_lossy().trim_end_matches('/'));
    assert_eq!(t.uploaded, 12_345);

    let half_id = half_meta.id();
    let t = node_wait(&half_id, "the partial torrent loaded", &|t| t.state == TorrentState::StoppedDownloading && t.progress > 0.0);
    let expected = 65_536.0 / 200_000.0;
    assert!((t.progress - expected).abs() < 1e-6, "piece 0 carried over without a download: {}", t.progress);
    app.shutdown();
}
