//! Integration tests with a real Python interpreter (skipped when none is
//! installed): install -> capabilities -> search -> results -> stop/delete,
//! downloads through `nova2dl.py`, updates from a local plugin index.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::app::{AppConfig, Profile};
use crate::events::EventBus;
use crate::logger::Logger;
use crate::prefs::{Preferences, PreferencesStore};

struct Harness {
    tmp: tempfile::TempDir,
    mgr: Arc<SearchManager>,
    added: Arc<Mutex<Vec<TorrentSource>>>,
}

impl Harness {
    async fn new(options: Options) -> Option<Self> {
        let tmp = tempfile::tempdir().unwrap();
        let config = AppConfig { profile_dir: tmp.path().join("profile").to_string_lossy().into_owned(), ..Default::default() };
        let profile = Arc::new(Profile::resolve(&config));
        for d in [&profile.config_dir, &profile.data_dir, &profile.cache_dir] {
            std::fs::create_dir_all(d).unwrap();
        }
        let prefs = Arc::new(PreferencesStore::in_memory(Preferences::default()));
        let events = EventBus::default();
        let logger = Arc::new(Logger::new(&profile, &prefs, events.clone(), false));
        let ctx = CoreContext {
            rt: tokio::runtime::Handle::current(),
            profile,
            prefs,
            events,
            logger,
            config: Arc::new(config),
        };
        let added: Arc<Mutex<Vec<TorrentSource>>> = Arc::default();
        let sink = added.clone();
        let adder: TorrentAdder = Arc::new(move |source, _params| {
            sink.lock().push(source);
            Box::pin(async { Ok("0123456789abcdef0123456789abcdef01234567".to_string()) })
        });
        let mgr = SearchManager::launch_with(&ctx, adder, options).await.unwrap();
        if !mgr.status().python_found {
            eprintln!("skipping search integration test: no Python 3.9+ found");
            mgr.shutdown().await;
            return None;
        }
        let h = Self { tmp, mgr, added };
        h.barrier().await;
        Some(h)
    }

    /// Waits until every queued plugin operation has run.
    async fn barrier(&self) {
        let (tx, rx) = oneshot::channel();
        self.mgr.inner.send(Op::Barrier { reply: tx }).unwrap();
        rx.await.unwrap();
    }

    fn write_source(&self, file: &str, content: &str) -> String {
        let dir = self.tmp.path().join("sources");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn plugin(&self, name: &str) -> Option<SearchPlugin> {
        self.mgr.plugins().into_iter().find(|p| p.name == name)
    }

    async fn wait_stopped(&self, id: i64, within: Duration) -> SearchJobInfo {
        let start = std::time::Instant::now();
        loop {
            let info = self.mgr.job_status(Some(id)).unwrap().remove(0);
            if info.status == SearchJobStatus::Stopped {
                return info;
            }
            assert!(start.elapsed() < within, "search {id} did not stop in time: {info:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

fn dummy_plugin(version: &str, url: &str, token: &str) -> String {
    format!(
        r#"# VERSION: {version}
# Test plugin returning fixed results.

import os
import tempfile

from novaprinter import prettyPrinter


class dummy(object):
    url = '{url}'
    name = 'Dummy Engine'
    supported_categories = {{'all': '0', 'movies': '6'}}

    def search(self, what, cat='all'):
        for i in range(3):
            prettyPrinter({{
                'link': 'magnet:?xt=urn:btih:%040d' % i,
                'name': 'Result %d %s | piped' % (i, what),
                'size': '1.5 GB',
                'seeds': 10 + i,
                'leech': i,
                'engine_url': self.url,
                'desc_link': '{url}/t/%d' % i,
                'pub_date': 1700000000 + i,
            }})

    def download_torrent(self, url):
        fd, path = tempfile.mkstemp(prefix='qbitneorstest{token}', suffix='.torrent')
        with os.fdopen(fd, 'wb') as f:
            f.write(b'd4:infod4:name4:testee')
        print(path + ' ' + url)
"#
    )
}

/// Prints one result, records the pid of the process running the search (a
/// multiprocessing worker, i.e. a grandchild) and then hangs.
fn slow_plugin(pid_file: &Path) -> String {
    format!(
        r#"# VERSION: 1.00
import os
import sys
import time

from novaprinter import prettyPrinter


class slow(object):
    url = 'http://127.0.0.1:9'
    name = 'Slow Engine'
    supported_categories = {{'all': '0'}}

    def search(self, what, cat='all'):
        with open({pid_file:?}, 'w') as f:
            f.write(str(os.getpid()))
        prettyPrinter({{'link': 'magnet:?xt=urn:btih:1', 'name': 'first', 'size': 1, 'seeds': 1, 'leech': 1,
                       'engine_url': self.url}})
        sys.stdout.flush()
        time.sleep(120)
"#,
        pid_file = pid_file.to_string_lossy()
    )
}

const EVIL_PLUGIN: &str = r#"# VERSION: 1.00
import os
import tempfile


class evil(object):
    url = 'http://127.0.0.1:9'
    name = 'Evil Engine'
    supported_categories = {'all': '0'}

    def search(self, what, cat='all'):
        pass

    def download_torrent(self, url):
        target = url.split('#', 1)[1]
        link = os.path.join(tempfile.mkdtemp(), 'link.torrent')
        os.symlink(target, link)
        print(link + ' ' + url)
"#;

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn python_end_to_end() {
    let options = Options { fetch_icons: false, ..Default::default() };
    let Some(h) = Harness::new(options).await else { return };
    let token = format!("{:08x}", rand::random::<u32>());
    let engines = h.mgr.inner.dirs.engines.clone();
    assert!(h.mgr.plugins().is_empty());

    // ---- Install --------------------------------------------------------------
    let dummy = h.write_source("dummy.py", &dummy_plugin("1.00", "http://127.0.0.1:9", &token));
    h.mgr.install_plugins(vec![dummy.clone()]).await.unwrap();
    let p = h.plugin("dummy").expect("dummy installed");
    assert_eq!(p.full_name, "Dummy Engine");
    assert_eq!(p.version, "1.0");
    assert!(p.enabled);
    let cats: Vec<&str> = p.supported_categories.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(cats, ["all", "movies"]);
    assert!(engines.join("dummy.py").is_file());

    // Same version again, invalid code, missing header, wrong extension, bad name.
    let err = h.mgr.install_plugins(vec![dummy.clone()]).await.unwrap_err().to_string();
    assert!(err.contains("A more recent version of this plugin is already installed."), "{err}");
    let broken = h.write_source("broken.py", "# VERSION: 1.00\ndef oops(:\n");
    let err = h.mgr.install_plugins(vec![broken]).await.unwrap_err().to_string();
    assert!(err.contains("Couldn't install \"broken\"") && err.contains("not valid Python"), "{err}");
    let nover = h.write_source("nover.py", "class nover(object):\n    pass\n");
    assert!(h.mgr.install_plugins(vec![nover]).await.unwrap_err().to_string().contains("VERSION"));
    let txt = h.write_source("plugin.txt", "# VERSION: 1.00\n");
    assert!(h.mgr.install_plugins(vec![txt]).await.unwrap_err().to_string().contains("Unknown search engine plugin file format."));
    let bad = h.write_source("bad-name.py", "# VERSION: 1.00\n");
    assert!(h.mgr.install_plugins(vec![bad]).await.is_err());
    let unsupported = h.write_source("notengine.py", "# VERSION: 1.00\nx = 1\n");
    let err = h.mgr.install_plugins(vec![unsupported]).await.unwrap_err().to_string();
    assert!(err.contains("Plugin is not supported."), "{err}");
    assert!(!engines.join("notengine.py").exists());
    assert!(h.mgr.install_plugins(vec!["ftp://example.com/x.py".into()]).await.is_err());
    assert!(h.mgr.install_plugins(vec!["relative/x.py".into()]).await.is_err());
    // Nothing staged is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&engines)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());

    // An update replaces the plugin and its version.
    let dummy2 = h.write_source("dummy.py", &dummy_plugin("1.10", "http://127.0.0.1:9", &token));
    h.mgr.install_plugins(vec![dummy2]).await.unwrap();
    assert_eq!(h.plugin("dummy").unwrap().version, "1.10");
    assert!(!engines.join("dummy.py.bak").exists());

    // ---- Search -------------------------------------------------------------------
    let id = h.mgr.start("  foo bar ", &["enabled".into()], "all").unwrap();
    let info = h.wait_stopped(id, Duration::from_secs(60)).await;
    assert_eq!(info.outcome, SearchJobOutcome::Finished, "{info:?}");
    assert_eq!(info.total, 3);
    assert_eq!(info.pattern, "foo bar");

    let page = h.mgr.results(id, 0, 0).unwrap();
    assert_eq!(page.status, SearchJobStatus::Stopped);
    assert_eq!(page.total, 3);
    let mut results = page.results;
    results.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    let r = &results[0];
    assert_eq!(r.file_name, "Result 0 foo%20bar   piped");
    assert_eq!(r.file_url, format!("magnet:?xt=urn:btih:{:040}", 0));
    assert_eq!(r.file_size, 1_610_612_736);
    assert_eq!(r.seeders, 10);
    assert_eq!(r.leechers, 0);
    assert_eq!(r.engine_name, "dummy");
    assert_eq!(r.site_url, "http://127.0.0.1:9");
    assert_eq!(r.descr_link, "http://127.0.0.1:9/t/0");
    assert_eq!(r.pub_date, 1_700_000_000);

    assert_eq!(h.mgr.results(id, 1, -1).unwrap().results.len(), 1);
    assert_eq!(h.mgr.results(id, 2, 1).unwrap().results.len(), 2);
    assert!(matches!(h.mgr.results(id, 0, 4), Err(QbtError::Conflict { .. })));
    assert!(matches!(h.mgr.results(id, 0, -4), Err(QbtError::Conflict { .. })));

    // Unsupported category: the plugin is skipped.
    let id2 = h.mgr.start("x", &["dummy".into()], "Music").unwrap();
    let info = h.wait_stopped(id2, Duration::from_secs(60)).await;
    assert_eq!(info.outcome, SearchJobOutcome::NoResults, "{info:?}");

    // Argument validation.
    assert!(matches!(h.mgr.start("x", &["all".into()], "bogus"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(h.mgr.start(" \0\t ", &["all".into()], "all"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(h.mgr.start("x", &["../x".into()], "all"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(h.mgr.start("x", &["a".into(), "-c".into()], "all"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(h.mgr.stop(42), Err(QbtError::NotFound { .. })));
    assert!(matches!(h.mgr.results(42, 0, 0), Err(QbtError::NotFound { .. })));
    assert_eq!(h.mgr.job_status(None).unwrap().len(), 2);
    assert_eq!(h.mgr.job_status(Some(0)).unwrap().len(), 2);

    // ---- Download -----------------------------------------------------------------
    h.mgr.download_torrent("magnet:?xt=urn:btih:abc", "dummy", AddTorrentParams::default()).await.unwrap();
    h.mgr.download_torrent("http://127.0.0.1:9/file.torrent", "dummy", AddTorrentParams::default()).await.unwrap();
    // Unknown plugin: the URL is handed to the session directly.
    h.mgr.download_torrent("https://example.invalid/a.torrent", "", AddTorrentParams::default()).await.unwrap();
    assert!(h.mgr.download_torrent("file:///etc/hosts", "dummy", AddTorrentParams::default()).await.is_err());
    assert!(h.mgr.download_torrent("javascript:alert(1)", "dummy", AddTorrentParams::default()).await.is_err());
    {
        let added = h.added.lock();
        assert_eq!(added.len(), 3);
        assert_eq!(added[0], TorrentSource::Url { url: "magnet:?xt=urn:btih:abc".into() });
        assert_eq!(added[1], TorrentSource::Bytes { data: b"d4:infod4:name4:testee".to_vec() });
        assert_eq!(added[2], TorrentSource::Url { url: "https://example.invalid/a.torrent".into() });
    }
    // The temporary file was deleted.
    let leftover = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with(&format!("qbitneorstest{token}")));
    assert!(!leftover, "nova2dl temp file not removed");

    // A plugin returning a symlink to another file is refused and the target kept.
    let evil = h.write_source("evil.py", EVIL_PLUGIN);
    h.mgr.install_plugins(vec![evil]).await.unwrap();
    let secret = h.tmp.path().join("secret.torrent");
    std::fs::write(&secret, b"d4:infod4:name6:secretee").unwrap();
    let url = format!("http://127.0.0.1:9/x#{}", secret.display());
    let err = h.mgr.download_torrent(&url, "evil", AddTorrentParams::default()).await.unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");
    assert!(secret.is_file());
    assert_eq!(h.added.lock().len(), 3);

    // ---- Enable / disable -------------------------------------------------------
    h.mgr.enable_plugins(&["dummy".into(), "unknown".into()], false).unwrap();
    assert!(!h.plugin("dummy").unwrap().enabled);
    h.barrier().await;
    let cfg = std::fs::read_to_string(&h.mgr.inner.config_file).unwrap();
    assert!(cfg.contains("\"dummy\"") && !cfg.contains("unknown"), "{cfg}");
    assert_eq!(h.mgr.inner.resolve_plugins(&["enabled".into()]).unwrap(), vec!["evil".to_string()]);
    assert_eq!(h.mgr.inner.resolve_plugins(&["ALL".into()]).unwrap(), vec!["dummy".to_string(), "evil".to_string()]);
    h.mgr.enable_plugins(&["dummy".into()], true).unwrap();
    assert!(h.plugin("dummy").unwrap().enabled);

    // ---- Stop / delete --------------------------------------------------------------
    let pid_file = h.tmp.path().join("slow.pid");
    let slow = h.write_source("slow.py", &slow_plugin(&pid_file));
    h.mgr.install_plugins(vec![slow]).await.unwrap();
    let id3 = h.mgr.start("slow", &["slow".into()], "all").unwrap();
    // Results stream in while the process runs.
    let start = std::time::Instant::now();
    while h.mgr.job_status(Some(id3)).unwrap()[0].total < 1 {
        assert!(start.elapsed() < Duration::from_secs(60), "no streamed result");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(h.mgr.job_status(Some(id3)).unwrap()[0].status, SearchJobStatus::Running);
    let worker_pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(pid_alive(worker_pid));
    h.mgr.stop(id3).unwrap();
    let info = h.wait_stopped(id3, Duration::from_secs(10)).await;
    assert_eq!(info.outcome, SearchJobOutcome::Aborted);
    assert_eq!(info.total, 1);
    // The multiprocessing worker (a grandchild) is gone too.
    let start = std::time::Instant::now();
    while pid_alive(worker_pid) {
        assert!(start.elapsed() < Duration::from_secs(10), "worker process {worker_pid} survived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    h.mgr.delete(id3).unwrap();
    assert!(matches!(h.mgr.job_status(Some(id3)), Err(QbtError::NotFound { .. })));
    assert!(matches!(h.mgr.delete(id3), Err(QbtError::NotFound { .. })));
    assert_eq!(h.mgr.job_status(None).unwrap().len(), 2);

    // Delete while running kills the process as well.
    let id4 = h.mgr.start("slow", &["slow".into()], "all").unwrap();
    let start = std::time::Instant::now();
    while !pid_file.exists() || h.mgr.job_status(Some(id4)).unwrap()[0].total < 1 {
        assert!(start.elapsed() < Duration::from_secs(60));
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let worker_pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    h.mgr.delete(id4).unwrap();
    let start = std::time::Instant::now();
    while pid_alive(worker_pid) {
        assert!(start.elapsed() < Duration::from_secs(10), "worker process {worker_pid} survived delete");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ---- Concurrency cap -------------------------------------------------------------
    let mut running = Vec::new();
    for _ in 0..MAX_RUNNING_JOBS {
        running.push(h.mgr.start("slow", &["slow".into()], "all").unwrap());
    }
    let err = h.mgr.start("slow", &["slow".into()], "all").unwrap_err();
    assert!(err.to_string().contains("Unable to create more than 5 concurrent searches."), "{err}");
    for id in &running {
        h.mgr.stop(*id).unwrap();
    }
    for id in &running {
        h.wait_stopped(*id, Duration::from_secs(15)).await;
    }

    // ---- Uninstall ----------------------------------------------------------------------
    std::fs::write(engines.join("dummy.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(engines.join("dummy_extra.py"), b"# VERSION: 1.00\n").unwrap();
    h.mgr.uninstall_plugins(&["dummy".into(), "../evil".into()]).unwrap();
    assert!(h.plugin("dummy").is_none());
    h.barrier().await;
    assert!(!engines.join("dummy.py").exists());
    assert!(!engines.join("dummy.png").exists());
    assert!(engines.join("dummy_extra.py").exists(), "only <name>.* is removed");
    assert!(h.plugin("evil").is_some());

    h.mgr.shutdown().await;
    assert!(h.mgr.start("x", &["all".into()], "all").is_err());
}


#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_timeout_aborts() {
    let options = Options { fetch_icons: false, search_timeout: Duration::from_secs(2), ..Default::default() };
    let Some(h) = Harness::new(options).await else { return };
    let pid_file = h.tmp.path().join("slow.pid");
    let slow = h.write_source("slow.py", &slow_plugin(&pid_file));
    h.mgr.install_plugins(vec![slow]).await.unwrap();
    let id = h.mgr.start("x", &["all".into()], "all").unwrap();
    let info = h.wait_stopped(id, Duration::from_secs(20)).await;
    assert_eq!(info.outcome, SearchJobOutcome::Aborted);
    h.mgr.shutdown().await;
}

/// Minimal HTTP/1.1 server answering GETs from a path -> body map.
fn serve(listener: tokio::net::TcpListener, files: HashMap<String, Vec<u8>>) {
    let files = Arc::new(files);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let files = files.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => req.extend_from_slice(&buf[..n]),
                    }
                }
                let line = String::from_utf8_lossy(&req).lines().next().unwrap_or_default().to_string();
                let path = line.split(' ').nth(1).unwrap_or("/").to_string();
                let (status, body) = match files.get(&path) {
                    Some(b) => ("200 OK", b.clone()),
                    None => ("404 Not Found", b"not found".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn install_from_url_update_and_icons() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    let mut files = HashMap::new();
    files.insert("/favicon.ico".to_string(), png.clone());
    files.insert("/engines/versions.txt".to_string(), b"# index\ndummy: 1.20\n".to_vec());
    files.insert("/engines/dummy.py".to_string(), dummy_plugin("1.20", &site, "upd").into_bytes());
    files.insert("/direct/dummy.py".to_string(), dummy_plugin("1.05", &site, "upd").into_bytes());
    serve(listener, files);

    let options = Options { update_url: format!("{site}/engines/"), fetch_icons: true, ..Default::default() };
    let Some(h) = Harness::new(options).await else { return };

    // Install from a URL, then update from the index.
    h.mgr.install_plugins(vec![format!("{site}/direct/dummy.py")]).await.unwrap();
    assert_eq!(h.plugin("dummy").unwrap().version, "1.5");
    let err = h.mgr.install_plugins(vec![format!("{site}/missing/other.py")]).await.unwrap_err();
    assert!(err.to_string().contains("Failed to download the plugin file."), "{err}");
    let mut events = h.mgr.inner.ctx.events.subscribe();
    h.mgr.update_plugins().await.unwrap();
    assert_eq!(h.plugin("dummy").unwrap().version, "1.20");
    let mut messages = Vec::new();
    while let Ok(ev) = events.try_recv() {
        if let CoreEvent::SearchPluginsUpdated { message } = ev {
            messages.push(message);
        }
    }
    assert!(messages.iter().any(|m| m == "Plugins installed or updated: dummy"), "{messages:?}");
    // Up to date now.
    h.mgr.update_plugins().await.unwrap();

    // Listing plugins (done above) fetches the favicon in the background.
    let start = std::time::Instant::now();
    loop {
        let icon = h.plugin("dummy").unwrap().icon_path;
        if !icon.is_empty() {
            assert!(icon.ends_with("dummy.png"), "{icon}");
            assert_eq!(std::fs::read(&icon).unwrap(), png);
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "icon not fetched");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    h.mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_update_index_fails() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut files = HashMap::new();
    files.insert("/engines/versions.txt".to_string(), b"dummy 1.20\nok: 1.0\n".to_vec());
    serve(listener, files);
    let options = Options { update_url: format!("{site}/engines/"), fetch_icons: false, ..Default::default() };
    let Some(h) = Harness::new(options).await else { return };
    let err = h.mgr.update_plugins().await.unwrap_err().to_string();
    assert!(err.contains("Incorrect update info received for 1 out of 2 plugins."), "{err}");
    h.mgr.shutdown().await;
}
