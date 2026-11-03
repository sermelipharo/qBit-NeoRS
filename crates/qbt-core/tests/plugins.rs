//! qBittorrent 5.3's example plugins (tests/fixtures/plugins, unchanged)
//! running against a real session with local data only: no network, and
//! e-mail goes through the real mail code path with no server configured.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qbt_core::model::*;
use qbt_core::prefs::Preferences;
use qbt_core::{App, AppConfig};

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
    p.advanced.plugins_enabled = true;
    // No SMTP server: sending fails before any connection is made.
    p.downloads.mail_notification_enabled = false;
    p.downloads.mail_notification_smtp = String::new();
    std::fs::create_dir_all(profile.join("config")).unwrap();
    std::fs::write(profile.join("config/preferences.json"), serde_json::to_vec_pretty(&p).unwrap()).unwrap();
}

fn make_torrent(path: &Path) -> Vec<u8> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
        let t = librqbit::create_torrent(
            path,
            librqbit::CreateTorrentOptions { name: None, trackers: vec![], piece_length: Some(32 * 1024) },
            &spawner,
        )
        .await
        .unwrap();
        t.as_bytes().unwrap().to_vec()
    })
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins")
}

fn logs(app: &App) -> Vec<String> {
    app.main_log(-1, vec![]).into_iter().map(|m| m.message).collect()
}

fn wait_log(app: &App, needle: &str) {
    let start = Instant::now();
    while !logs(app).iter().any(|l| l.contains(needle)) {
        assert!(start.elapsed() < Duration::from_secs(30), "no log line {needle:?}: {:#?}", logs(app));
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn upstream_example_plugins() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = downloads.join("Album");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("01.bin"), vec![7u8; 100_000]).unwrap();
    std::fs::write(data.join("02.bin"), vec![9u8; 50_000]).unwrap();
    let torrent = make_torrent(&data);

    let app: Arc<App> =
        App::new(AppConfig { profile_dir: profile.to_string_lossy().into_owned(), front_end: "test".into(), ..Default::default() })
            .expect("App::new");
    let rt = app.runtime();
    wait_until("plugins engine", || app.plugins_status().running);

    for name in ["EmailNotifier", "EventHandler", "Invocable", "RunExternalProgram"] {
        let path = fixtures().join(format!("{name}.lua"));
        let r = rt.block_on(app.install_plugin(path.to_string_lossy().into_owned())).expect(name);
        assert_eq!(r.plugin.id, name);
        assert_eq!(r.plugin.version, "0.1");
        rt.block_on(app.set_plugin_enabled(name.into(), true)).unwrap();
    }
    let status = app.plugins_status();
    let names: Vec<(&str, bool)> = status.plugins.iter().map(|p| (p.name.as_str(), p.invocable)).collect();
    assert_eq!(
        names,
        [
            ("Email Notifier Example", false),
            ("Event Handler Example", false),
            ("Invocable Example", true),
            ("Run External Program Example", false)
        ]
    );
    assert_eq!(status.plugins[1].handlers.len(), 21);

    rt.block_on(app.invoke_plugin("Invocable".into())).unwrap();
    wait_log(&app, "Invocable Example: Invoked.");

    let hash = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent },
            AddTorrentParams { save_path: Some(downloads.to_string_lossy().into()), ..Default::default() },
        ))
        .expect("add");
    wait_log(&app, "Event Handler Example: Torrent 'Album' is added.");
    wait_log(&app, "Event Handler Example: Torrent 'Album' has metadata ready.");
    // RunExternalProgram: `touch "<save path>/<name>.txt"`, without a shell.
    let marker = downloads.join("Album.txt");
    wait_until("touch", || marker.exists());
    wait_log(&app, "Plugin 'RunExternalProgram' is running external program. Command: `touch");

    wait_until("seeding", || app.torrent(hash.clone()).is_ok_and(|t| t.progress == 1.0));
    wait_log(&app, "Event Handler Example: Torrent 'Album' is finished.");
    // EmailNotifier reaches the mail code, which fails locally (no server).
    wait_log(&app, "Plugin 'EmailNotifier' is sending an e-mail. Subject: \"Torrent 'Album' has finished downloading\"");
    wait_log(&app, "Email Notification Error:");
    // RunExternalProgram's `/usr/bin/nemo` does not exist here.
    wait_log(&app, "Plugin 'RunExternalProgram' failed to run external program. Command: `/usr/bin/nemo");

    // Category, tags, stop/start.
    app.create_category(Category { name: "Music".into(), ..Default::default() }).unwrap();
    app.set_category(vec![hash.clone()], "Music".into()).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' has changed category.");
    app.add_tags(vec![hash.clone()], vec!["lua".into()]).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' has tag added.");
    app.remove_tags(vec![hash.clone()], vec!["lua".into()]).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' has tag removed.");
    rt.block_on(app.stop_torrents(vec![hash.clone()])).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' is stopped.");
    rt.block_on(app.start_torrents(vec![hash.clone()])).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' is started.");

    // Removing: the handler still reads the torrent, and `rm` deletes the marker.
    rt.block_on(app.remove_torrents(vec![hash.clone()], false)).unwrap();
    wait_log(&app, "Event Handler Example: Torrent 'Album' is about to be removed.");
    wait_until("rm", || !marker.exists());
    assert!(data.join("01.bin").exists());

    // Switching plugins off stops the engine and frees every Lua state.
    let mut p = app.preferences();
    p.advanced.plugins_enabled = false;
    app.set_preferences(p).unwrap();
    wait_until("engine stopped", || !app.plugins_status().running);
    app.shutdown();
}

/// A local HTTP tracker: `/ok/announce` answers, `/warn/announce` answers
/// with a warning message and `/fail/announce` with a failure reason.
fn serve_tracker() -> u16 {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..n]);
            let body = if request.starts_with("GET /fail/") {
                "d14:failure reason4:nopee"
            } else if request.starts_with("GET /warn/") {
                "d15:warning message4:slow8:intervali1800e5:peers0:e"
            } else {
                "d8:intervali1800e5:peers0:e"
            };
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    port
}

const PROBE: &str = r#"
NAME = "Probe"
VERSION = "1.0"

local updates = 0

function onTorrentsUpdated(torrents)
    updates = updates + 1
    if updates == 1 then
        qBittorrent.log(string.format("probe: updated %s (%d)", torrents[1].name, #torrents))
    end
end

function onTorrentAnnounceSuccess(torrent, url)
    qBittorrent.log("probe: success " .. url)
end

function onTorrentAnnounceWarning(torrent, url)
    qBittorrent.log("probe: warning " .. url)
end

function onTorrentAnnounceError(torrent, url)
    qBittorrent.log("probe: error " .. url)
end

function onTorrentTrackerStatusesUpdated(torrent, statuses)
    for url, status in pairs(statuses) do
        qBittorrent.log(string.format("probe: status %s %d '%s' of %s", url, status.state, status.message, torrent.name))
    end
end

function onTorrentContentFileRenamed(torrent, index, oldPath)
    qBittorrent.log(string.format("probe: renamed %d %s -> %s", index, oldPath, torrent:filePath(index)))
end

function invoke()
    qBittorrent.log("probe: updates " .. updates)
end
"#;

fn probe_updates(app: &App, rt: &tokio::runtime::Runtime) -> u64 {
    let before = logs(app).iter().filter(|l| l.contains("probe: updates ")).count();
    rt.block_on(app.invoke_plugin("Probe".into())).unwrap();
    wait_until("probe count", || logs(app).iter().filter(|l| l.contains("probe: updates ")).count() > before);
    let line = logs(app).into_iter().rfind(|l| l.contains("probe: updates ")).unwrap();
    line.rsplit(' ').next().unwrap().parse().unwrap()
}

/// `onTorrentsUpdated`, the announce events, tracker status updates and the
/// file renames of "Manage Torrent Content", against local trackers.
#[test]
fn torrent_update_tracker_and_rename_events() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("profile");
    let downloads = tmp.path().join("downloads");
    write_prefs(&profile, &downloads);
    let data = downloads.join("Probe");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("a.bin"), vec![1u8; 70_000]).unwrap();
    std::fs::write(data.join("b.bin"), vec![2u8; 30_000]).unwrap();
    let torrent = make_torrent(&data);
    let port = serve_tracker();

    let app: Arc<App> =
        App::new(AppConfig { profile_dir: profile.to_string_lossy().into_owned(), front_end: "test".into(), ..Default::default() })
            .expect("App::new");
    let rt = app.runtime();
    // The trackers are on 127.0.0.1.
    let mut p = app.preferences();
    p.advanced.ssrf_mitigation = false;
    app.set_preferences(p).unwrap();
    wait_until("plugins engine", || app.plugins_status().running);
    let file = tmp.path().join("Probe.lua");
    std::fs::write(&file, PROBE).unwrap();
    rt.block_on(app.install_plugin(file.to_string_lossy().into_owned())).unwrap();
    rt.block_on(app.set_plugin_enabled("Probe".into(), true)).unwrap();

    let hash = rt
        .block_on(app.add_torrent(
            TorrentSource::Bytes { data: torrent },
            AddTorrentParams { save_path: Some(downloads.to_string_lossy().into()), ..Default::default() },
        ))
        .expect("add");
    wait_until("seeding", || app.torrent(hash.clone()).is_ok_and(|t| t.progress == 1.0));
    wait_log(&app, "probe: updated Probe (1)");

    let url = |kind: &str| format!("http://127.0.0.1:{port}/{kind}/announce");
    let trackers = ["ok", "warn", "fail"]
        .iter()
        .enumerate()
        .map(|(tier, kind)| TrackerTierEntry { url: url(kind), tier: tier as i64 })
        .collect();
    app.add_trackers(hash.clone(), trackers).unwrap();
    wait_log(&app, &format!("probe: success {}", url("ok")));
    wait_log(&app, &format!("probe: warning {}", url("warn")));
    wait_log(&app, &format!("probe: success {}", url("warn")));
    wait_log(&app, &format!("probe: error {}", url("fail")));
    wait_log(&app, &format!("probe: status {} 2 '' of Probe", url("ok")));
    wait_log(&app, &format!("probe: status {} 2 'slow' of Probe", url("warn")));
    wait_log(&app, &format!("probe: status {} 5 'nope' of Probe", url("fail")));
    // Each result is reported once.
    std::thread::sleep(Duration::from_millis(600));
    let count = |needle: &str| logs(&app).iter().filter(|l| l.contains(needle)).count();
    assert_eq!(count(&format!("probe: success {}", url("ok"))), 1);
    assert_eq!(count(&format!("probe: error {}", url("fail"))), 1);
    assert_eq!(count(&format!("probe: warning {}", url("ok"))), 0);

    // Stopping resets the statuses (qBittorrent reports them before
    // onTorrentStopped).
    rt.block_on(app.stop_torrents(vec![hash.clone()])).unwrap();
    wait_log(&app, &format!("probe: status {} 1 '' of Probe", url("fail")));
    // A stopped torrent with nothing changing is not reported again.
    std::thread::sleep(Duration::from_millis(1000));
    let idle = probe_updates(&app, rt);
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(probe_updates(&app, rt), idle, "an idle torrent was reported as updated");

    // Manage Torrent Content: one event per renamed file, with its old path.
    let files = app.files(hash.clone(), vec![]).unwrap();
    let old = files.iter().find(|f| f.name.ends_with("a.bin")).unwrap();
    let new_path = old.name.replace("a.bin", "sub/c.bin");
    let failures = rt
        .block_on(app.rename_files(
            hash.clone(),
            vec![
                FileRename { index: old.index, new_path: new_path.clone() },
                FileRename { index: 99, new_path: "x".into() },
            ],
        ))
        .unwrap();
    assert_eq!(failures.len(), 1);
    wait_log(&app, &format!("probe: renamed {} {} -> {new_path}", old.index, old.name));
    assert_eq!(count("probe: renamed"), 1);

    app.shutdown();
}
