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
