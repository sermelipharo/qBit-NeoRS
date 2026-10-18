//! End-to-end tests of the RSS session: a local HTTP server serves feeds,
//! a test double stands in for the BitTorrent session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::app::{AppConfig, Profile};
use crate::events::EventBus;
use crate::logger::Logger;
use crate::prefs::PreferencesStore;

#[derive(Default)]
struct MockAdder {
    calls: Mutex<Vec<(String, AddTorrentParams)>>,
    /// Error returned for URLs containing the key.
    errors: Mutex<Vec<(String, QbtError)>>,
}

impl TorrentAdder for MockAdder {
    fn add_torrent_url(&self, url: String, params: AddTorrentParams) -> BoxFuture<'_, QbtResult<String>> {
        let err = self.errors.lock().iter().find(|(k, _)| url.contains(k.as_str())).map(|(_, e)| e.clone());
        self.calls.lock().push((url, params));
        Box::pin(async move {
            match err {
                Some(e) => Err(e),
                None => Ok("0123456789abcdef0123456789abcdef01234567".to_owned()),
            }
        })
    }
}

fn prefs_with(f: impl FnOnce(&mut RssPrefs)) -> Preferences {
    let mut p = Preferences::default();
    p.behavior.file_log_enabled = false;
    p.rss.rss_fetch_delay = 0;
    f(&mut p.rss);
    p
}

fn context(dir: &Path, prefs: Preferences) -> CoreContext {
    let config = AppConfig { profile_dir: dir.to_string_lossy().into_owned(), ..Default::default() };
    let profile = Arc::new(Profile::resolve(&config));
    let prefs = Arc::new(PreferencesStore::in_memory(prefs));
    let events = EventBus::default();
    let logger = Arc::new(Logger::new(&profile, &prefs, events.clone(), false));
    CoreContext { rt: tokio::runtime::Handle::current(), profile, prefs, events, logger, config: Arc::new(config) }
}

type Routes = Arc<Mutex<HashMap<String, (u16, Vec<u8>)>>>;

/// Minimal HTTP/1.1 server: `GET <path>` -> the route's status and body.
async fn serve(routes: Routes) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let routes = routes.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (status, body) = routes.lock().get(&path).cloned().unwrap_or((404, b"not found".to_vec()));
                let resp = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

fn find_feed<'a>(folder: &'a RssFolder, name: &str) -> Option<&'a RssFeed> {
    folder
        .feeds
        .iter()
        .find(|f| tree::relative_name(&f.path) == name)
        .or_else(|| folder.folders.iter().find_map(|sub| find_feed(sub, name)))
}

const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0"><channel>
  <title>Linux ISOs</title>
  <item>
    <title>Ubuntu 26.04 Desktop</title>
    <guid>ubuntu</guid>
    <pubDate>Sat, 03 Oct 2026 04:05:06 +0000</pubDate>
    <enclosure url="https://example.invalid/ubuntu.torrent" type="application/x-bittorrent"/>
  </item>
  <item>
    <title>Fedora 44 Workstation</title>
    <guid>fedora</guid>
    <pubDate>Fri, 02 Oct 2026 04:05:06 +0000</pubDate>
    <link>magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&amp;dn=fedora</link>
  </item>
  <item>
    <title>Debian 13</title>
    <guid>debian</guid>
    <pubDate>Thu, 01 Oct 2026 04:05:06 +0000</pubDate>
    <link>file:///etc/passwd</link>
  </item>
  <item>
    <title>Arch Linux</title>
    <guid>arch</guid>
    <pubDate>Wed, 30 Sep 2026 04:05:06 +0000</pubDate>
    <enclosure url="https://example.invalid/arch.torrent" type="application/x-bittorrent"/>
  </item>
</channel></rss>"#;

fn rule(name: &str, must: &str, feed_url: &str) -> RssAutoDownloadRule {
    RssAutoDownloadRule {
        name: name.into(),
        enabled: true,
        must_contain: must.into(),
        affected_feeds: vec![feed_url.into()],
        torrent_params: rule::default_template(),
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_parse_and_auto_download() {
    let dir = tempfile::tempdir().unwrap();
    let routes: Routes = Arc::default();
    routes.lock().insert("/feed.xml".into(), (200, FEED.as_bytes().to_vec()));
    let base = serve(routes.clone()).await;
    let url = format!("{base}/feed.xml");

    let ctx = context(
        dir.path(),
        prefs_with(|p| {
            p.rss_processing_enabled = true;
            p.rss_auto_downloading_enabled = true;
        }),
    );
    let mut events = ctx.events.subscribe();
    let adder = Arc::new(MockAdder::default());
    adder.errors.lock().push(("arch".into(), QbtError::conflict("duplicate torrent")));
    let mgr = RssManager::launch_with(&ctx, adder.clone()).await.unwrap();

    let mut ubuntu = rule("Ubuntu", "ubuntu", &url);
    ubuntu.torrent_params.category = "linux".into();
    ubuntu.torrent_params.tags = vec!["iso".into()];
    mgr.set_rule(ubuntu).unwrap();
    mgr.set_rule(rule("Fedora", "fedora", &url)).unwrap();
    mgr.set_rule(rule("Debian", "debian", &url)).unwrap();
    mgr.set_rule(rule("Arch", "arch", &url)).unwrap();

    mgr.add_feed(&url, "", 0).unwrap();
    assert!(matches!(mgr.add_feed(&url, "Other", 0), Err(QbtError::Conflict { .. })));

    wait_for("torrent adds", || adder.calls.lock().len() >= 3).await;
    wait_for("feed rename and read states", || {
        mgr.tree(false).feeds.first().is_some_and(|f| f.path == "Linux ISOs" && f.unread_count == 1)
    })
    .await;

    let calls = adder.calls.lock().clone();
    let urls: Vec<&str> = calls.iter().map(|(u, _)| u.as_str()).collect();
    assert!(urls.contains(&"https://example.invalid/ubuntu.torrent"));
    assert!(urls.contains(&"magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=fedora"));
    assert!(urls.contains(&"https://example.invalid/arch.torrent"));
    assert!(!urls.iter().any(|u| u.starts_with("file:")), "unsupported URLs are never added");
    let (_, params) = calls.iter().find(|(u, _)| u.contains("ubuntu")).unwrap();
    assert_eq!(params.category.as_deref(), Some("linux"));
    assert_eq!(params.tags, ["iso"]);

    // Read: Ubuntu (added), Fedora (magnet), Arch (duplicate). Unread: Debian.
    let articles = mgr.articles("Linux ISOs").unwrap();
    assert_eq!(articles.len(), 4);
    assert_eq!(articles[0].title, "Ubuntu 26.04 Desktop");
    let unread: Vec<_> = articles.iter().filter(|a| !a.is_read).map(|a| a.title.as_str()).collect();
    assert_eq!(unread, ["Debian 13"]);

    let feed = &mgr.tree(true).feeds[0];
    assert_eq!(feed.title, "Linux ISOs");
    assert!(!feed.is_loading && !feed.has_error);
    assert_eq!(feed.url, url);

    // Rules remember their last match.
    let rules = mgr.rules();
    assert_eq!(rules.iter().find(|r| r.name == "Ubuntu").unwrap().last_match, "03 Oct 2026 04:05:06 +0000");
    let matching = mgr.matching_articles("Ubuntu").unwrap();
    assert_eq!(matching, [RssMatchingArticles { feed_name: "Linux ISOs".into(), article_titles: vec!["Ubuntu 26.04 Desktop".into()] }]);
    assert!(mgr.matching_articles("missing").unwrap().is_empty());

    // Events: a download notification and (debounced) feed updates.
    let mut downloaded = Vec::new();
    let mut updated = false;
    wait_for("events", || {
        while let Ok(e) = events.try_recv() {
            match e {
                CoreEvent::RssTorrentDownloaded { rule, article_title } => downloaded.push((rule, article_title)),
                CoreEvent::RssUpdated => updated = true,
                _ => {}
            }
        }
        updated && downloaded.len() >= 2
    })
    .await;
    assert!(downloaded.contains(&("Ubuntu".into(), "Ubuntu 26.04 Desktop".into())));

    // Logs like qBittorrent.
    let logs: Vec<String> = ctx.logger.messages(-1, &[]).into_iter().map(|m| m.message).collect();
    assert!(logs.iter().any(|l| l == &format!("RSS feed at '{url}' is successfully downloaded. Starting to parse it.")));
    assert!(logs.iter().any(|l| l == &format!("RSS feed at '{url}' updated. Added 4 new articles.")));
    assert!(logs.iter().any(|l| l == "RSS article 'Ubuntu 26.04 Desktop' is accepted by rule 'Ubuntu'. Trying to add torrent..."));
    assert!(logs.iter().any(|l| l.starts_with("Failed to add torrent from RSS article. Reason: unsupported torrent URL.")));

    // A second refresh of the same content adds nothing and downloads nothing.
    let n = adder.calls.lock().len();
    mgr.refresh_item("").unwrap();
    wait_for("second parse", || {
        ctx.logger.messages(-1, &[]).iter().any(|m| m.message == format!("RSS feed at '{url}' updated. Added 0 new articles."))
    })
    .await;
    assert_eq!(adder.calls.lock().len(), n);

    mgr.shutdown().await;

    // Everything is persisted and reloads.
    let rss = dir.path().join("data/rss");
    assert!(rss.join("feeds.json").is_file());
    assert!(rss.join("download_rules.json").is_file());
    let ctx2 = context(dir.path(), prefs_with(|_| {}));
    let mgr2 = RssManager::launch_with(&ctx2, Arc::new(MockAdder::default())).await.unwrap();
    let feed2 = mgr2.tree(false).feeds[0].clone();
    assert_eq!(feed2.path, "Linux ISOs");
    assert_eq!(feed2.unread_count, 1);
    let articles2 = mgr2.articles("Linux ISOs").unwrap();
    assert_eq!(articles2, articles);
    assert_eq!(mgr2.rules_json(), mgr.rules_json());
    mgr2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_operations_and_items_json() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), prefs_with(|_| {}));
    let mgr = RssManager::launch_with(&ctx, Arc::new(MockAdder::default())).await.unwrap();

    mgr.add_folder("TV").unwrap();
    mgr.add_folder("TV\\Shows").unwrap();
    assert_eq!(
        mgr.add_folder("TV").unwrap_err(),
        QbtError::conflict("RSS item with given path already exists: TV.")
    );
    assert_eq!(mgr.add_folder("Nope\\X").unwrap_err(), QbtError::conflict("Parent folder doesn't exist: Nope."));
    mgr.add_feed("https://a.invalid/rss", "TV\\Shows\\A", 900).unwrap();
    mgr.add_feed("https://b.invalid/rss", "", 0).unwrap();
    assert!(mgr.add_feed("file:///etc/passwd", "Local", 0).is_err());
    assert!(mgr.add_feed("javascript:alert(1)", "", 0).is_err());

    // Feed named after its URL follows URL changes.
    mgr.set_feed_url("https://b.invalid/rss", "https://c.invalid/rss").unwrap();
    assert!(mgr.tree(false).feeds.iter().any(|f| f.path == "https://c.invalid/rss"));
    mgr.set_feed_url("TV\\Shows\\A", "https://a2.invalid/rss").unwrap();
    assert_eq!(
        mgr.set_feed_url("TV", "https://x.invalid/").unwrap_err(),
        QbtError::conflict("Feed doesn't exist: TV.")
    );
    assert!(mgr.set_feed_url("https://c.invalid/rss", "https://a2.invalid/rss").is_err());

    mgr.set_feed_refresh_interval("TV\\Shows\\A", 60).unwrap();
    assert!(mgr.set_feed_refresh_interval("TV\\Shows\\A", -1).is_err());

    // Move/rename.
    mgr.move_item("TV", "Television").unwrap();
    assert_eq!(
        mgr.move_item("Television", "Television\\Shows\\Inner").unwrap_err(),
        QbtError::conflict("Can't move a folder into itself or its subfolders.")
    );
    mgr.move_item("https://c.invalid/rss", "C feed").unwrap();

    let tree = mgr.tree(false);
    assert_eq!(tree.folders.len(), 1);
    assert_eq!(tree.folders[0].path, "Television");
    assert_eq!(tree.folders[0].folders[0].feeds[0].path, "Television\\Shows\\A");
    assert_eq!(tree.folders[0].folders[0].feeds[0].refresh_interval, 60);
    assert_eq!(tree.feeds[0].path, "C feed");

    let items = mgr.items_json(false);
    let feed_a = &items["Television"]["Shows"]["A"];
    assert_eq!(feed_a["url"], "https://a2.invalid/rss");
    assert_eq!(feed_a["refreshInterval"], 60);
    assert!(feed_a["uid"].as_str().unwrap().starts_with('{'));
    assert!(feed_a.get("articles").is_none());
    assert!(items["C feed"].get("refreshInterval").is_none());
    let with_data = mgr.items_json(true);
    assert_eq!(with_data["C feed"]["articles"], serde_json::json!([]));
    assert_eq!(with_data["C feed"]["isLoading"], false);

    // Unknown items are ignored by mark-as-read/refresh; articles() reports them.
    mgr.mark_as_read("nope", None).unwrap();
    mgr.refresh_item("nope").unwrap();
    assert!(matches!(mgr.articles("nope"), Err(QbtError::NotFound { .. })));

    mgr.remove_item("Television").unwrap();
    assert_eq!(mgr.remove_item("").unwrap_err(), QbtError::conflict("Cannot delete root folder."));
    assert_eq!(mgr.remove_item("Television").unwrap_err(), QbtError::conflict("Item doesn't exist: Television."));
    let before = mgr.items_json(false);
    mgr.shutdown().await;

    let mgr2 = RssManager::launch_with(&context(dir.path(), prefs_with(|_| {})), Arc::new(MockAdder::default()))
        .await
        .unwrap();
    assert_eq!(mgr2.items_json(false), before);
    mgr2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_refresh_errors_read_state_and_trimming() {
    let dir = tempfile::tempdir().unwrap();
    let routes: Routes = Arc::default();
    routes.lock().insert("/feed.xml".into(), (200, FEED.as_bytes().to_vec()));
    routes.lock().insert("/broken.xml".into(), (200, b"<rss><channel><item><title>x</title></item>".to_vec()));
    let base = serve(routes.clone()).await;
    let ctx = context(dir.path(), prefs_with(|_| {}));
    let mgr = RssManager::launch_with(&ctx, Arc::new(MockAdder::default())).await.unwrap();

    mgr.add_folder("F").unwrap();
    mgr.add_feed(&format!("{base}/feed.xml"), "F\\Good", 0).unwrap();
    mgr.add_feed(&format!("{base}/missing.xml"), "F\\Missing", 0).unwrap();
    mgr.add_feed(&format!("{base}/broken.xml"), "F\\Broken", 0).unwrap();
    // Processing is disabled: nothing is fetched automatically.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(mgr.articles("F").unwrap().is_empty());

    mgr.refresh_item("F").unwrap();
    wait_for("refresh results", || {
        let t = mgr.tree(false);
        let good = find_feed(&t, "Good").unwrap();
        let missing = find_feed(&t, "Missing").unwrap();
        let broken = find_feed(&t, "Broken").unwrap();
        good.unread_count == 4 && missing.has_error && !missing.is_loading && broken.has_error && !broken.is_loading
    })
    .await;
    let t = mgr.tree(false);
    assert!(!find_feed(&t, "Good").unwrap().has_error);
    // Articles before the XML error are kept.
    assert_eq!(find_feed(&t, "Broken").unwrap().unread_count, 1);
    let logs: Vec<String> = ctx.logger.messages(-1, &[LogLevel::Warning]).into_iter().map(|m| m.message).collect();
    assert!(logs.iter().any(|l| l.starts_with(&format!("Failed to download RSS feed at '{base}/missing.xml'. Reason:"))));
    assert!(logs.iter().any(|l| l.starts_with(&format!("Failed to parse RSS feed at '{base}/broken.xml'. Reason:"))));

    // Read state: one article, then a folder recursively.
    let id = mgr.articles("F\\Good").unwrap()[0].id.clone();
    mgr.mark_as_read("F\\Good", Some(&id)).unwrap();
    mgr.mark_as_read("F\\Good", Some("unknown id")).unwrap();
    assert_eq!(find_feed(&mgr.tree(false), "Good").unwrap().unread_count, 3);
    mgr.mark_as_read("F", None).unwrap();
    let t = mgr.tree(false);
    assert_eq!(find_feed(&t, "Good").unwrap().unread_count, 0);
    assert_eq!(find_feed(&t, "Broken").unwrap().unread_count, 0);

    // Lowering the article cap drops the oldest articles.
    ctx.prefs.update(|p| p.rss.rss_max_articles_per_feed = 2).unwrap();
    wait_for("trim", || mgr.articles("F\\Good").map(|a| a.len() == 2).unwrap_or(false)).await;
    let titles: Vec<String> = mgr.articles("F\\Good").unwrap().into_iter().map(|a| a.title).collect();
    assert_eq!(titles, ["Ubuntu 26.04 Desktop", "Fedora 44 Workstation"]);
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn processing_switch_and_timer() {
    let dir = tempfile::tempdir().unwrap();
    let routes: Routes = Arc::default();
    routes.lock().insert("/feed.xml".into(), (200, FEED.as_bytes().to_vec()));
    let base = serve(routes.clone()).await;
    let ctx = context(dir.path(), prefs_with(|_| {}));
    let mgr = RssManager::launch_with(&ctx, Arc::new(MockAdder::default())).await.unwrap();
    // A 1-second per-feed interval.
    mgr.add_feed(&format!("{base}/feed.xml"), "Feed", 1).unwrap();
    assert!(mgr.articles("Feed").unwrap().is_empty());

    // Enabling processing refreshes immediately.
    ctx.prefs.update(|p| p.rss.rss_processing_enabled = true).unwrap();
    wait_for("first refresh", || mgr.articles("Feed").map(|a| a.len() == 4).unwrap_or(false)).await;

    // New content shows up through the per-feed timer.
    let updated = FEED.replace("</channel>", "<item><title>New one</title><guid>new</guid></item></channel>");
    routes.lock().insert("/feed.xml".into(), (200, updated.into_bytes()));
    wait_for("timer refresh", || mgr.articles("Feed").map(|a| a.len() == 5).unwrap_or(false)).await;

    // Disabling stops the timer.
    ctx.prefs.update(|p| p.rss.rss_processing_enabled = false).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let count = ctx.logger.messages(-1, &[]).len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let refreshes_after = ctx.logger.messages(-1, &[]).len();
    assert_eq!(count, refreshes_after, "no refresh while disabled");
    mgr.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rule_management_import_export() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path(), prefs_with(|_| {}));
    let mgr = RssManager::launch_with(&ctx, Arc::new(MockAdder::default())).await.unwrap();

    let mut a = rule("A", "show*720p|other", "https://a.invalid/rss");
    a.priority = 2;
    a.episode_filter = "1x2-;".into();
    a.smart_filter = true;
    a.previously_matched_episodes = vec!["1x1".into()];
    mgr.set_rule(a).unwrap();
    mgr.set_rule(rule("B", "", "https://b.invalid/rss")).unwrap();
    assert!(mgr.set_rule(rule("", "", "")).is_err());
    assert_eq!(mgr.rules().iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["B", "A"], "priority order");

    assert!(matches!(mgr.rename_rule("B", "A"), Err(QbtError::Conflict { .. })));
    assert!(matches!(mgr.rename_rule("missing", "C"), Err(QbtError::NotFound { .. })));
    mgr.rename_rule("B", "C").unwrap();
    mgr.clear_rule_history("A").unwrap();
    assert!(mgr.rules().iter().find(|r| r.name == "A").unwrap().previously_matched_episodes.is_empty());
    assert!(mgr.clear_rule_history("missing").is_err());

    // WebAPI-style setRule: JSON definition, defaults for invalid JSON.
    mgr.set_rule_json("D", r#"{"mustContain":"x","affectedFeeds":"https://d.invalid/rss","enabled":false}"#).unwrap();
    mgr.set_rule_json("E", "not json").unwrap();
    let json = mgr.rules_json();
    assert_eq!(json["D"]["affectedFeeds"], serde_json::json!(["https://d.invalid/rss"]));
    assert_eq!(json["D"]["enabled"], false);
    assert_eq!(json["E"]["enabled"], true);
    assert_eq!(json["E"]["torrentParams"]["operating_mode"], "AutoManaged");

    let exported = mgr.export_rules();
    let legacy = mgr.export_rules_legacy();
    mgr.remove_rule("missing").unwrap();
    for name in ["A", "C", "D", "E"] {
        mgr.remove_rule(name).unwrap();
    }
    assert!(mgr.rules().is_empty());
    assert_eq!(mgr.export_rules(), b"{}\n");

    mgr.import_rules(&exported).unwrap();
    assert_eq!(mgr.rules_json(), json);

    for name in ["A", "C", "D", "E"] {
        mgr.remove_rule(name).unwrap();
    }
    mgr.import_rules(&legacy).unwrap();
    let names: Vec<String> = mgr.rules().into_iter().map(|r| r.name).collect();
    assert_eq!(names.len(), 4);
    assert_eq!(mgr.rules_json()["A"]["mustContain"], "show*720p|other");

    assert!(matches!(mgr.import_rules(b"{ not json"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(mgr.import_rules(b"{\"x\": 5}"), Err(QbtError::InvalidArgument { .. })));
    assert!(matches!(mgr.import_rules(b"\x00\x00\x00\x05garbage"), Err(QbtError::InvalidArgument { .. })));
    assert!(mgr.import_rules(&vec![b' '; 11 * 1024 * 1024]).is_err());

    // Live preview of an unsaved rule needs no feed data.
    assert!(mgr.matching_articles_for(&rule("tmp", "x", "https://none.invalid/")).is_empty());
    assert_eq!(validate_rule_expression("a[", false), "Invalid wildcard: a[");
    assert_eq!(validate_rule_expression("a b|c*", false), "");
    assert!(!validate_rule_expression("(", true).is_empty());
    mgr.shutdown().await;

    // download_rules.json is written in qBittorrent's format.
    let data = std::fs::read(dir.path().join("data/rss/download_rules.json")).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&data).unwrap();
    assert_eq!(v["A"]["episodeFilter"], "1x2-;");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corrupted_files_do_not_prevent_start() {
    let dir = tempfile::tempdir().unwrap();
    let rss = dir.path().join("data/rss");
    std::fs::create_dir_all(rss.join("articles")).unwrap();
    std::fs::write(rss.join("feeds.json"), b"{\"A\": {\"uid\": \"{6b8c1234-0000-4000-8000-0000000000ab}\", \"url\": \"https://a.invalid/\"}, \"B\": 5}").unwrap();
    std::fs::write(rss.join("articles/6b8c12340000400080000000000000ab.json"), b"[{\"id\": 1, \"date\": \"garbage\"}, \"x\"]").unwrap();
    std::fs::write(rss.join("download_rules.json"), b"[]").unwrap();
    let ctx = context(dir.path(), prefs_with(|_| {}));
    let mgr = RssManager::launch_with(&ctx, Arc::new(MockAdder::default())).await.unwrap();
    let t = mgr.tree(false);
    assert_eq!(t.feeds.len(), 1);
    assert_eq!(t.feeds[0].uid, "{6b8c1234-0000-4000-8000-0000000000ab}");
    assert_eq!(mgr.articles("A").unwrap().len(), 1);
    assert!(mgr.rules().is_empty());
    let logs: Vec<String> = ctx.logger.messages(-1, &[]).into_iter().map(|m| m.message).collect();
    assert!(logs.iter().any(|l| l.starts_with("Couldn't load RSS AutoDownloader rules. Reason:")));
    assert!(logs.iter().any(|l| l.contains("Invalid data format")));
    mgr.shutdown().await;
}
