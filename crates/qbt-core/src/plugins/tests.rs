//! Unit tests of the plugins engine: sandbox, budgets, memory cap,
//! metadata/version rule, install/uninstall/enable and event dispatch
//! against a fake host.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::host::{PluginHost, ShareLimitsValue, TorrentAction};
use super::runtime::{self, Limits};
use super::*;
use crate::model::{ShareLimitAction, ShareLimitsMode, TorrentInfo, TrackerEntry};
use crate::net::testutil;
use crate::prefs::Preferences;

#[derive(Default)]
struct FakeHost {
    actions: Mutex<Vec<(String, TorrentAction)>>,
    mails: Mutex<Vec<(String, String)>>,
}

fn sample_info() -> TorrentInfo {
    TorrentInfo {
        hash: "ab".repeat(20),
        infohash_v1: "ab".repeat(20),
        name: "Sample".into(),
        has_metadata: true,
        save_path: "/dl".into(),
        content_path: "/dl/Sample".into(),
        root_path: "/dl/Sample".into(),
        category: "Movies".into(),
        tags: vec!["a".into(), "b".into()],
        total_size: 3000,
        size: 3000,
        piece_size: 1024,
        pieces_num: 3,
        progress: 0.5,
        queue_position: 2,
        ..Default::default()
    }
}

impl PluginHost for FakeHost {
    fn torrent_info(&self, id: &str) -> Option<TorrentInfo> {
        (id == "ab".repeat(20)).then(sample_info)
    }
    fn files(&self, _id: &str) -> Vec<(String, i64)> {
        vec![("Sample/a.bin".into(), 1000), ("Sample/b.bin".into(), 2000)]
    }
    fn trackers(&self, _id: &str) -> Vec<TrackerEntry> {
        vec![TrackerEntry { url: "http://t/announce".into(), tier: 0, ..Default::default() }]
    }
    fn url_seeds(&self, _id: &str) -> Vec<String> {
        vec!["http://seed/".into()]
    }
    fn apply(&self, id: &str, action: TorrentAction) -> bool {
        self.actions.lock().push((id.to_string(), action));
        true
    }
    fn global_share_limits(&self) -> (ShareLimitsMode, ShareLimitAction) {
        (ShareLimitsMode::MatchAny, ShareLimitAction::Stop)
    }
    fn send_mail(&self, _plugin_id: &str, subject: String, body: String) {
        self.mails.lock().push((subject, body));
    }
}

fn quick() -> Limits {
    Limits { timeout: Duration::from_millis(200), max_instructions: 50_000_000, memory: 4 * 1024 * 1024 }
}

fn load(src: &str) -> Result<runtime::Metadata, String> {
    runtime::load(src.as_bytes(), "test.lua", &quick()).map(|(_, _, m)| m)
}

fn load_with(src: &str, limits: &Limits) -> Result<runtime::Metadata, String> {
    runtime::load(src.as_bytes(), "t.lua", limits).map(|(_, _, m)| m)
}

const HEADER: &str = "NAME = 'Test'\nVERSION = '0.1'\n";

#[test]
fn metadata_and_version_rule() {
    let m = load("NAME = 'X'\nVERSION = '1.2'\nAUTHOR = 'Me'\nDESCRIPTION = 'Does things'").unwrap();
    assert_eq!((m.name.as_str(), m.version.to_string().as_str(), m.author.as_str()), ("X", "1.2", "Me"));
    assert_eq!(m.description, "Does things");
    let m = load("NAME = 42\nVERSION = 0.5").unwrap();
    assert_eq!((m.name.as_str(), m.version.to_string().as_str()), ("42", "0.5"));
    assert_eq!(load("VERSION = '0.1'").unwrap_err(), "Plugin name is missing or invalid.");
    assert_eq!(load("NAME = ''\nVERSION = '0.1'").unwrap_err(), "Plugin name is missing or invalid.");
    for v in ["'1'", "'1.2.3'", "'0.0'", "'x.y'", "{}", "nil"] {
        assert_eq!(load(&format!("NAME = 'X'\nVERSION = {v}")).unwrap_err(), "Plugin version is missing or invalid.", "{v}");
    }
    assert!(load("this is not lua").is_err());
}

#[test]
fn sandbox_hides_unsafe_functions() {
    let src = format!(
        "{HEADER}
        local names = {{'io','os','debug','package','require','load','loadfile','dofile','collectgarbage',
            'getmetatable','setmetatable','rawget','rawset','rawequal','rawlen','warn','ffi','jit'}}
        for _, n in ipairs(names) do
            if _G[n] ~= nil then error('visible: ' .. n) end
        end
        if string.dump ~= nil then error('visible: string.dump') end
        assert(string.format and table.concat and math.floor and utf8.char and coroutine.create)
        assert(qBittorrent == nil, 'API visible to the main chunk')"
    );
    load(&src).unwrap();
}

#[test]
fn bytecode_is_refused() {
    let err = load("\x1bLua\x54\x00garbage").unwrap_err();
    assert!(err.contains("binary") || err.contains("attempt to load"), "{err}");
}

#[test]
fn infinite_loops_are_stopped() {
    let start = Instant::now();
    assert_eq!(load(&format!("{HEADER} while true do end")).unwrap_err(), "Timed out.");
    assert!(start.elapsed() < Duration::from_secs(3));
    // pcall cannot swallow the timeout.
    let err = load(&format!("{HEADER} while true do pcall(function() while true do end end) end")).unwrap_err();
    assert_eq!(err, "Timed out.");
    let err = load(&format!("{HEADER} while true do xpcall(function() while true do end end, function(e) return e end) end")).unwrap_err();
    assert_eq!(err, "Timed out.");
    // Nor can a coroutine.
    let err = load(&format!(
        "{HEADER} while true do coroutine.resume(coroutine.create(function() while true do end end)) end"
    ))
    .unwrap_err();
    assert_eq!(err, "Timed out.");
    // The instruction cap stops a loop even with a long timeout.
    let limits = Limits { timeout: Duration::from_secs(60), max_instructions: 1_000_000, memory: 4 << 20 };
    let start = Instant::now();
    assert_eq!(load_with(&format!("{HEADER} while true do end"), &limits).unwrap_err(), "Timed out.");
    assert!(start.elapsed() < Duration::from_secs(10));
    // pcall still catches ordinary errors.
    load(&format!("{HEADER} local ok, e = pcall(error, 'boom') assert(not ok and e == 'boom')")).unwrap();
}

#[test]
fn memory_is_capped() {
    let err = load(&format!("{HEADER} local t = {{}} for i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end")).unwrap_err();
    assert_eq!(err, "Not enough memory.");
    let err = load(&format!("{HEADER} local s = string.rep('x', 64 * 1024 * 1024)")).unwrap_err();
    assert_eq!(err, "Not enough memory.");
    // Not catchable either.
    let err = load(&format!("{HEADER} pcall(string.rep, 'x', 64 * 1024 * 1024) error('caught')")).unwrap_err();
    assert_eq!(err, "Not enough memory.");
}

/// A running engine in a temporary profile, with the fake host.
struct Engine {
    rt: tokio::runtime::Runtime,
    ctx: crate::context::CoreContext,
    manager: Arc<PluginsManager>,
    host: Arc<FakeHost>,
    _dir: tempfile::TempDir,
    src: tempfile::TempDir,
}

fn engine(enabled: bool) -> Engine {
    let dir = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let rt = testutil::runtime();
    let mut prefs = Preferences::default();
    prefs.advanced.plugins_enabled = enabled;
    let ctx = testutil::context_with(&rt, dir.path(), prefs);
    let host = Arc::new(FakeHost::default());
    let manager = {
        let _g = rt.enter();
        PluginsManager::launch_with(&ctx, host.clone(), quick())
    };
    Engine { rt, ctx, manager, host, _dir: dir, src }
}

impl Engine {
    fn write(&self, name: &str, body: &str) -> String {
        let p = self.src.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().into_owned()
    }
    fn install(&self, name: &str, body: &str) -> crate::error::QbtResult<PluginInstallResult> {
        let p = self.write(name, body);
        self.rt.block_on(self.manager.install(p))
    }
    fn enable(&self, id: &str) {
        self.rt.block_on(self.manager.set_enabled(id.into(), true)).unwrap();
    }
    fn logs(&self) -> Vec<String> {
        self.ctx.logger.messages(-1, &[]).into_iter().map(|m| m.message).collect()
    }
    fn wait_log(&self, needle: &str) -> Vec<String> {
        let start = Instant::now();
        loop {
            let logs = self.logs();
            if logs.iter().any(|l| l.contains(needle)) {
                return logs;
            }
            assert!(start.elapsed() < Duration::from_secs(10), "no log line {needle:?}: {logs:#?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn torrent_event(&self, handler: PluginHandler, extra: Vec<PluginArg>) {
        let mut args = vec![PluginArg::Torrent(TorrentArg { id: "ab".repeat(20), captured: None })];
        args.extend(extra);
        self.ctx.plugins.emit(handler, args, &self.ctx.logger);
    }
}

fn wait_status(m: &PluginsManager, pred: impl Fn(&PluginsStatus) -> bool) -> PluginsStatus {
    let start = Instant::now();
    loop {
        let s = m.status();
        if pred(&s) {
            return s;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "status never matched: {s:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.manager.stop();
    }
}

#[test]
fn off_means_no_engine() {
    let e = engine(false);
    let s = e.manager.status();
    assert!(!s.running && s.plugins.is_empty());
    assert!(!e.ctx.plugins.wants(PluginHandler::TorrentAdded));
    let err = e.install("A.lua", HEADER).unwrap_err();
    assert!(err.to_string().contains("Plugins are turned off"), "{err}");
    assert!(!e._dir.path().join("data/plugins").exists(), "folder created while off");
}

#[test]
fn install_update_enable_uninstall() {
    let e = engine(true);
    let r = e.install("Inv.lua", "NAME = 'Invocable'\nVERSION = '0.1'\nfunction invoke() qBittorrent.log('invoked!') end").unwrap();
    assert!(!r.updated && !r.plugin.enabled && r.plugin.invocable);
    assert_eq!(r.plugin.id, "Inv");
    let installed = e._dir.path().join("data/plugins/Inv.lua");
    assert!(installed.exists());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777, 0o600);

    // Invoking a disabled plugin is refused.
    assert!(e.rt.block_on(e.manager.invoke("Inv".into())).is_err());
    e.enable("Inv");
    e.rt.block_on(e.manager.invoke("Inv".into())).unwrap();
    e.wait_log("invoked!");
    let cfg: serde_json::Value = serde_json::from_slice(&std::fs::read(e._dir.path().join("config/plugins.json")).unwrap()).unwrap();
    assert_eq!(cfg["Inv"]["enabled"], true);

    // Same and older versions are refused as update failures.
    let same = e.install("Inv.lua", "NAME = 'Invocable'\nVERSION = '0.1'").unwrap_err();
    assert!(matches!(same, crate::error::QbtError::Conflict { .. }) && same.to_string().contains("same version"), "{same}");
    let older = e.install("Inv.lua", "NAME = 'Invocable'\nVERSION = '0.0'").unwrap_err();
    assert!(older.to_string().contains("version is missing"), "{older}");
    let r = e.install("Inv.lua", "NAME = 'Invocable 2'\nVERSION = '0.2'").unwrap();
    assert!(r.updated && r.plugin.enabled && !r.plugin.invocable);
    assert_eq!(r.previous_version, "0.1");
    let older = e.install("Inv.lua", "NAME = 'Invocable'\nVERSION = '0.1'").unwrap_err();
    assert!(older.to_string().contains("newest version"), "{older}");

    // Bad files.
    let bad = e.install("Bad.lua", "NAME = 'x'").unwrap_err();
    assert!(matches!(bad, crate::error::QbtError::InvalidArgument { .. }), "{bad}");
    assert!(e.install("notes.txt", HEADER).is_err());
    assert!(e.rt.block_on(e.manager.install("relative/Inv.lua".into())).is_err());
    let big = format!("{HEADER}--{}", "x".repeat(fs::MAX_PLUGIN_SIZE as usize));
    assert!(e.install("Big.lua", &big).unwrap_err().to_string().contains("exceeds limit"));

    assert_eq!(e.manager.status().plugins.len(), 1);
    e.rt.block_on(e.manager.uninstall("Inv".into())).unwrap();
    assert!(!installed.exists());
    assert!(e.manager.status().plugins.is_empty());
    assert!(e.rt.block_on(e.manager.uninstall("Inv".into())).is_err());
    let logs = e.logs();
    for line in ["Installed plugin. ID: Inv. Version: 0.1.", "Updated plugin. ID: Inv. Version: 0.2.", "Uninstalled plugin. ID: Inv.", "Enabled plugin. ID: Inv."] {
        assert!(logs.iter().any(|l| l == line), "missing {line:?}: {logs:#?}");
    }
}

#[test]
fn state_survives_restart_and_switch() {
    let e = engine(true);
    e.install("A.lua", &format!("{HEADER} function onTorrentAdded(t) end")).unwrap();
    e.enable("A");
    assert!(e.ctx.plugins.wants(PluginHandler::TorrentAdded));
    assert!(!e.ctx.plugins.wants(PluginHandler::TorrentFinished));
    e.ctx.prefs.update(|p| p.advanced.plugins_enabled = false).unwrap();
    let start = Instant::now();
    while e.manager.status().running {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!e.ctx.plugins.wants(PluginHandler::TorrentAdded));
    e.ctx.prefs.update(|p| p.advanced.plugins_enabled = true).unwrap();
    let start = Instant::now();
    while e.manager.status().plugins.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
    let s = e.manager.status();
    assert!(s.running && s.plugins[0].enabled);
    assert_eq!(s.plugins[0].handlers, ["onTorrentAdded"]);
}

#[test]
fn unsafe_files_do_not_load() {
    use std::os::unix::fs::PermissionsExt;
    let e = engine(true);
    e.install("A.lua", HEADER).unwrap();
    e.manager.stop();
    let f = e._dir.path().join("data/plugins/A.lua");
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o666)).unwrap();
    std::fs::write(e._dir.path().join("data/plugins/B.lua"), "NAME = 'B'\nVERSION = '0.1'").unwrap();
    std::fs::set_permissions(e._dir.path().join("data/plugins/B.lua"), std::fs::Permissions::from_mode(0o600)).unwrap();
    let e2 = {
        let _g = e.rt.enter();
        PluginsManager::launch_with(&e.ctx, e.host.clone(), quick())
    };
    let s = wait_status(&e2, |s| s.running);
    assert_eq!(s.plugins.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["B"]);
    assert!(s.problems.iter().any(|p| p.contains("A.lua") && p.contains("writable by other users")), "{:?}", s.problems);
    e2.stop();

    // A folder others can write to loads nothing.
    let dir = e._dir.path().join("data/plugins");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    let e3 = {
        let _g = e.rt.enter();
        PluginsManager::launch_with(&e.ctx, e.host.clone(), quick())
    };
    let s = wait_status(&e3, |s| s.running);
    assert!(s.plugins.is_empty() && !s.problems.is_empty());
    assert!(e.rt.block_on(e3.install(e.write("C.lua", HEADER))).is_err());
    e3.stop();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn events_reach_handlers_with_the_torrent_api() {
    let e = engine(true);
    let src = r#"
NAME = "API"
VERSION = "1.0"
function onTorrentAdded(t)
    qBittorrent.log(string.format("added %s %s %d %d %s %s", t.name, t.id, t.filesCount, t.queuePosition, table.concat(t.tags, ","), t.savePath))
    qBittorrent.log(string.format("file %s %d %s %s", t:filePath(1), t:fileSize(1), t.trackerStatuses[1].url, t.urlSeeds[1]))
    qBittorrent.log(string.format("limits %d %d %s", t.effectiveShareLimits.mode, t.shareLimits.action, tostring(t:hasTag("a"))))
    qBittorrent.log("size " .. qBittorrent.friendlySizeUnit(t.totalSize) .. " " .. qBittorrent.friendlyDuration(3700))
    t.name = "Renamed"
    t.category = "TV"
    t:addTag("new")
    t:removeTag("a")
    local l = qBittorrent.ShareLimits()
    l.ratioLimit = 2.5
    l.action = qBittorrent.ShareLimitAction.Remove
    t.shareLimits = l
    t:stop()
    qBittorrent.sendMail("subj", "body")
end
function onTorrentCategoryChanged(t, old) qBittorrent.log("category was " .. old) end
function onTorrentAboutToBeRemoved(t) qBittorrent.log("removing " .. t.name) end
function onTorrentTrackersAdded(t, list) qBittorrent.log("tracker " .. list[1].url .. " " .. list[1].tier) end
function onTorrentContentFolderRenamed(t, new, old, files) qBittorrent.log("folder " .. old .. ">" .. new .. " " .. files[0]) end
"#;
    e.install("API.lua", src).unwrap();
    e.enable("API");
    e.torrent_event(PluginHandler::TorrentAdded, vec![]);
    let logs = e.wait_log("size ");
    let id = "ab".repeat(20);
    assert!(logs.contains(&format!("added Sample {id} 2 1 a,b /dl")), "{logs:#?}");
    assert!(logs.contains(&"file Sample/b.bin 2000 http://t/announce http://seed/".to_string()), "{logs:#?}");
    assert!(logs.contains(&"limits 0 -1 true".to_string()), "{logs:#?}");
    assert!(logs.contains(&"size 2.9\u{a0}KiB 1h 1m".to_string()), "{logs:#?}");
    let start = Instant::now();
    while e.host.mails.lock().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    let actions: Vec<TorrentAction> = e.host.actions.lock().iter().map(|(_, a)| a.clone()).collect();
    assert_eq!(
        actions,
        vec![
            TorrentAction::Rename("Renamed".into()),
            TorrentAction::SetCategory("TV".into()),
            TorrentAction::AddTag("new".into()),
            TorrentAction::RemoveTag("a".into()),
            TorrentAction::SetShareLimits(ShareLimitsValue { ratio_limit: 2.5, action: ShareLimitAction::Remove, ..Default::default() }),
            TorrentAction::Stop,
        ]
    );
    assert_eq!(e.host.mails.lock()[0], ("subj".to_string(), "body".to_string()));

    e.torrent_event(PluginHandler::TorrentCategoryChanged, vec![PluginArg::Str("Old".into())]);
    e.wait_log("category was Old");
    e.torrent_event(PluginHandler::TorrentTrackersAdded, vec![PluginArg::Trackers(vec![crate::model::TrackerTierEntry { url: "udp://x".into(), tier: 2 }])]);
    e.wait_log("tracker udp://x 2");
    e.torrent_event(
        PluginHandler::TorrentContentFolderRenamed,
        vec![PluginArg::Str("N".into()), PluginArg::Str("O".into()), PluginArg::IndexedPaths(vec![(0, "O/a".into())])],
    );
    e.wait_log("folder O>N O/a");

    // A torrent that is gone is read from the copy taken with the event.
    let mut gone = sample_info();
    gone.hash = "cd".repeat(20);
    gone.name = "Gone".into();
    let args = vec![PluginArg::Torrent(TorrentArg { id: gone.hash.clone(), captured: Some(Arc::new(gone)) })];
    e.ctx.plugins.emit(PluginHandler::TorrentAboutToBeRemoved, args, &e.ctx.logger);
    e.wait_log("removing Gone");

    // Handlers the plugin does not define are not even queued.
    assert!(!e.ctx.plugins.wants(PluginHandler::TorrentFinished));
    let info = &e.manager.status().plugins[0];
    assert_eq!(info.mails_sent, 1);
    assert_eq!(
        info.handlers,
        ["onTorrentAdded", "onTorrentCategoryChanged", "onTorrentContentFolderRenamed", "onTorrentAboutToBeRemoved", "onTorrentTrackersAdded"]
    );
}

#[test]
fn faulty_callbacks_are_contained() {
    let e = engine(true);
    let src = r#"
NAME = "Faulty"
VERSION = "0.1"
function onTorrentAdded(t) while true do end end
function onTorrentFinished(t) for i = 1, 1000 do qBittorrent.log("line " .. i) end end
function onTorrentStopped(t) for i = 1, 20 do qBittorrent.exec("/usr/bin/true") end end
function onTorrentStarted(t) error("broken") end
"#;
    e.install("Faulty.lua", src).unwrap();
    e.enable("Faulty");
    e.torrent_event(PluginHandler::TorrentStarted, vec![]);
    e.wait_log("Failed to call the plugin. Plugin: Faulty. Reason: Faulty.lua:7: broken");
    e.torrent_event(PluginHandler::TorrentFinished, vec![]);
    let logs = e.wait_log("wrote too many log lines");
    assert_eq!(logs.iter().filter(|l| l.starts_with("line ")).count(), api::MAX_LOG_LINES as usize);
    e.torrent_event(PluginHandler::TorrentStopped, vec![]);
    let logs = e.wait_log("tried to run too many programs");
    assert_eq!(logs.iter().filter(|l| l.contains("is running external program")).count(), api::MAX_EXECS as usize);
    for _ in 0..3 {
        e.torrent_event(PluginHandler::TorrentAdded, vec![]);
    }
    e.wait_log("was disabled because it ran out of time 3 times in a row");
    let s = e.manager.status();
    assert!(!s.plugins[0].enabled && s.plugins[0].failures >= 4);
    assert!(!e.ctx.plugins.wants(PluginHandler::TorrentAdded));
}

#[test]
fn full_queue_drops_events() {
    let e = engine(true);
    e.install("Slow.lua", "NAME='S'\nVERSION='0.1'\nfunction onTorrentAdded(t) local x = 0 while x < 1e6 do x = x + 1 end end").unwrap();
    e.enable("Slow");
    for _ in 0..(EVENT_QUEUE_CAPACITY * 2) {
        e.torrent_event(PluginHandler::TorrentAdded, vec![]);
    }
    assert!(e.ctx.plugins.dropped_events() > 0);
    e.wait_log("Plugins are too slow; dropped event \"onTorrentAdded\"");
}
