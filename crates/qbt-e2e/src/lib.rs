//! Helpers for end-to-end tests: isolated profiles with networking limited to
//! localhost (no DHT/LSD/UPnP), test data and polling utilities.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use qbt_core::model::{TorrentInfo, TorrentState};
use qbt_core::prefs::Preferences;
use qbt_core::{App, AppConfig};

pub struct Node {
    pub app: Arc<App>,
    pub dir: tempfile::TempDir,
    pub port: u16,
}

impl Node {
    /// Starts an App on a fresh profile listening on `port` (TCP+uTP) with
    /// every discovery mechanism except manual peers disabled.
    pub fn start(port: u16, tweak: impl FnOnce(&mut Preferences)) -> Node {
        let dir = tempfile::tempdir().expect("tempdir");
        let profile = dir.path().join("profile");
        let downloads = dir.path().join("downloads");
        std::fs::create_dir_all(profile.join("config")).unwrap();
        std::fs::create_dir_all(&downloads).unwrap();

        let mut prefs = Preferences::default();
        prefs.downloads.save_path = downloads.to_string_lossy().into_owned();
        prefs.downloads.temp_path = downloads.join("temp").to_string_lossy().into_owned();
        prefs.connection.listen_port = port as i64;
        prefs.connection.upnp = false;
        prefs.bittorrent.dht = false;
        prefs.bittorrent.lsd = false;
        prefs.bittorrent.pex = false;
        prefs.behavior.file_log_enabled = false;
        prefs.advanced.resolve_peer_countries = false;
        tweak(&mut prefs);
        std::fs::write(profile.join("config/preferences.json"), serde_json::to_vec_pretty(&prefs).unwrap()).unwrap();

        let app = App::new(AppConfig {
            profile_dir: profile.to_string_lossy().into_owned(),
            front_end: "e2e".into(),
            ..Default::default()
        })
        .expect("App::new");
        Node { app, dir, port }
    }

    pub fn downloads(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }

    pub fn torrent(&self, hash: &str) -> Option<TorrentInfo> {
        self.app.torrent(hash.to_string()).ok()
    }

    /// Polls until `pred` holds for the torrent or the timeout expires.
    pub fn wait_for(&self, hash: &str, timeout: Duration, what: &str, pred: impl Fn(&TorrentInfo) -> bool) -> TorrentInfo {
        let start = Instant::now();
        loop {
            if let Some(t) = self.torrent(hash)
                && pred(&t)
            {
                return t;
            }
            if start.elapsed() > timeout {
                panic!("timed out waiting for {what}; last state: {:?}", self.torrent(hash));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.app.shutdown();
    }
}

/// Writes deterministic pseudo-random content (multi-file torrent layout).
pub fn make_content(root: &Path, name: &str, files: &[(&str, usize)]) -> PathBuf {
    let base = root.join(name);
    for (i, (rel, size)) in files.iter().enumerate() {
        let p = base.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15 ^ (i as u64 + 1);
        let data: Vec<u8> = (0..*size)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        std::fs::write(p, data).unwrap();
    }
    base
}

/// Builds a v1 .torrent for `path` with the engine's creator.
pub fn make_torrent(path: &Path, piece_length: u32) -> Vec<u8> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let spawner = librqbit::spawn_utils::BlockingSpawner::new(2);
        let t = librqbit::create_torrent(
            path,
            librqbit::CreateTorrentOptions { name: None, trackers: vec![], piece_length: Some(piece_length) },
            &spawner,
        )
        .await
        .expect("create_torrent");
        t.as_bytes().expect("bencode").to_vec()
    })
}

/// Builds a v1, v2 or hybrid .torrent for `path` with the core's creator
/// (libtorrent-compatible layout: sorted files, BEP 52 merkle trees).
pub fn make_torrent_format(path: &Path, piece_length: u32, format: qbt_core::model::TorrentFormat) -> Vec<u8> {
    use qbt_core::creator::builder::{BuildParams, build_torrent};
    let params = BuildParams {
        source: path,
        format,
        piece_size: i64::from(piece_length),
        optimize_alignment: false,
        padding_file_limit: -1,
        private: false,
        trackers: &[],
        url_seeds: &[],
        comment: "",
        source_tag: "",
        created_by: "qbt-e2e",
        creation_date: 0,
        max_torrent_size: 64 << 20,
    };
    let cancel = std::sync::atomic::AtomicBool::new(false);
    build_torrent(&params, &cancel, &mut |_| {}).expect("build_torrent").data
}

pub fn is_seeding(t: &TorrentInfo) -> bool {
    matches!(t.state, TorrentState::Uploading | TorrentState::StalledUploading | TorrentState::ForcedUploading)
        && (t.progress - 1.0).abs() < f64::EPSILON
}

/// Picks a free TCP port on localhost.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub fn same_tree(a: &Path, b: &Path) -> bool {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.push((p.strip_prefix(root).unwrap().to_path_buf(), std::fs::read(&p).unwrap()));
            }
        }
    }
    let (mut x, mut y) = (Vec::new(), Vec::new());
    walk(a, a, &mut x);
    walk(b, b, &mut y);
    x.sort();
    y.sort();
    x == y
}
