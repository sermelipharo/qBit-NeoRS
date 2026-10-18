//! RSS reader and auto-downloader (qBittorrent `base/rss`): feeds and folders
//! tree, RSS 2.0/Atom/RDF parsing, article storage under `data/rss/articles`,
//! periodic refresh with per-host delay, and the auto-download rules engine
//! (must/must-not contain with wildcards or regex, episode filter, smart
//! episode filter, ignore days, per-rule torrent parameters).
//!
//! Files (`data/rss/`): `feeds.json` (item tree), `articles/<uid>.json`
//! (per-feed articles, `<uid>.ico` favicon) and `download_rules.json`.
//! Feed content is untrusted: see `docs/SECURITY.md` (RSS).
//!
//! Concurrency: all state lives behind one mutex. Methods compute their side
//! effects (file writes, log lines, downloads, events) while locked and
//! perform them after unlocking, so no callback ever runs under the lock.

pub(crate) mod date;
mod entities;
mod feed;
mod fetch;
mod legacy;
mod parser;
mod rule;
mod storage;
mod tree;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Map, Value};
use tokio::sync::{Notify, watch};
use tokio::task::{AbortHandle, JoinHandle};
use tokio::time::Instant;

use crate::bittorrent::Session;
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;
use crate::prefs::{Preferences, RssPrefs};

use feed::{Feed, Uid, is_supported_torrent_url, is_torrent_descriptor};
use rule::{MatchContext, Rule};
use storage::Store;
use tree::{ItemRef, Tree};

pub use rule::is_valid_episode_filter;

const FEEDS_FILE: &str = "feeds.json";
const RULES_FILE: &str = "download_rules.json";
const ARTICLES_DIR: &str = "articles";
const MAX_CONFIG_FILE: u64 = 10 * 1024 * 1024;
const MAX_ARTICLES_FILE: u64 = 64 * 1024 * 1024;
const MAX_RULE_NAME: usize = 1024;
const DEFERRED_SAVE: Duration = Duration::from_secs(5);
const UPDATE_DEBOUNCE: Duration = Duration::from_millis(250);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How the auto-downloader adds torrents. Implemented by the BitTorrent
/// session; tests substitute a double.
pub trait TorrentAdder: Send + Sync {
    /// Adds a torrent from an HTTP(S) URL, magnet URI or info hash. A
    /// `QbtError::Conflict` means "already in the session" (the article is
    /// then marked read, like qBittorrent's duplicate handling).
    fn add_torrent_url(&self, url: String, params: AddTorrentParams) -> BoxFuture<'_, QbtResult<String>>;
}

impl TorrentAdder for Session {
    fn add_torrent_url(&self, url: String, params: AddTorrentParams) -> BoxFuture<'_, QbtResult<String>> {
        Box::pin(self.add_torrent(TorrentSource::Url { url }, params))
    }
}

/// A queued auto-download job (snapshot of the article).
#[derive(Clone, Debug)]
struct Job {
    feed_uid: Uid,
    feed_url: String,
    article_id: String,
    title: String,
    date: i64,
    torrent_url: String,
}

struct FetchRequest {
    uid: Uid,
    url: String,
    generation: u64,
    want_icon: bool,
}

struct AddRequest {
    url: String,
    params: AddTorrentParams,
    rule: String,
    title: String,
    /// Mark the article read when the add completes (non-descriptor URLs).
    waiting: bool,
}

/// Side effects collected under the lock and performed afterwards.
#[derive(Default)]
struct Effects {
    logs: Vec<(LogLevel, String)>,
    save_feeds: bool,
    save_articles: Vec<Uid>,
    save_rules: bool,
    deferred_save: bool,
    writes: Vec<(PathBuf, Vec<u8>)>,
    removes: Vec<PathBuf>,
    fetches: Vec<FetchRequest>,
    aborts: Vec<Uid>,
    adds: Vec<AddRequest>,
    events: Vec<CoreEvent>,
    changed: bool,
    reschedule: bool,
    kick_jobs: bool,
}

impl Effects {
    fn normal(&mut self, m: String) {
        self.logs.push((LogLevel::Normal, m));
    }
    fn warning(&mut self, m: String) {
        self.logs.push((LogLevel::Warning, m));
    }
}

struct State {
    tree: Tree,
    /// Sorted by ascending priority (stable).
    rules: Vec<Rule>,
    match_ctx: MatchContext,
    /// Preferences as last applied (to detect transitions).
    prefs: RssPrefs,
    /// The single-shot refresh timer (`Session::m_refreshTimer`).
    refresh_deadline: Option<Instant>,
    queue: VecDeque<Job>,
    queued: HashSet<(Uid, String)>,
    /// Jobs whose torrent URL is being added, by URL.
    waiting: HashMap<String, Job>,
    rules_dirty: bool,
}

pub struct RssManager {
    ctx: CoreContext,
    adder: Arc<dyn TorrentAdder>,
    weak: Weak<RssManager>,
    state: Mutex<State>,
    store: Store,
    hosts: fetch::HostQueue,
    rss_dir: PathBuf,
    articles_dir: PathBuf,
    update_notify: Notify,
    save_notify: Notify,
    sched_notify: Notify,
    job_notify: Notify,
    fetches: Mutex<HashMap<Uid, AbortHandle>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    shut_down: AtomicBool,
}

/// Data read from disk at start-up.
struct Loaded {
    tree: Tree,
    rules: Vec<Rule>,
    logs: Vec<(LogLevel, String)>,
    resave_feeds: bool,
}

fn effective_max(p: &RssPrefs) -> usize {
    p.rss_max_articles_per_feed.clamp(0, i64::from(i32::MAX)) as usize
}

fn global_interval(p: &RssPrefs) -> Duration {
    Duration::from_secs(p.rss_refresh_interval.clamp(1, 999_999) as u64 * 60)
}

fn effective_interval(feed: &Feed, p: &RssPrefs) -> Duration {
    if feed.refresh_interval > 0 { Duration::from_secs(feed.refresh_interval as u64) } else { global_interval(p) }
}

fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let len = std::fs::metadata(path)?.len();
    if len > cap {
        return Err(std::io::Error::other(format!("File size ({len}) exceeds limit ({cap})")));
    }
    std::fs::read(path)
}

/// `rulesFromJSON`.
fn rules_from_json(data: &[u8]) -> Result<Vec<Rule>, String> {
    let v: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let Value::Object(obj) = v else {
        return Err("Invalid data format.".to_owned());
    };
    let mut rules = Vec::with_capacity(obj.len());
    for (name, val) in &obj {
        let Value::Object(o) = val else {
            return Err("Invalid data format.".to_owned());
        };
        rules.push(Rule::from_json(o, name));
    }
    Ok(rules)
}

fn load_from_disk(rss_dir: &Path, articles_dir: &Path, max: usize) -> Loaded {
    let mut logs = Vec::new();
    let mut tree = Tree::default();
    let mut resave_feeds = false;

    let feeds_path = rss_dir.join(FEEDS_FILE);
    match read_capped(&feeds_path, MAX_CONFIG_FILE) {
        Ok(data) => match serde_json::from_slice::<Value>(&data) {
            Ok(Value::Object(obj)) => {
                let mut warnings = Vec::new();
                resave_feeds = tree.load_folder(&obj, "", &mut warnings);
                logs.extend(warnings.into_iter().map(|w| (LogLevel::Warning, w)));
            }
            Ok(_) => logs.push((
                LogLevel::Warning,
                format!("Failed to load RSS session data. File: \"{}\". Error: \"Invalid data format.\"", feeds_path.display()),
            )),
            Err(e) => logs.push((
                LogLevel::Warning,
                format!("Failed to parse RSS session data. File: \"{}\". Error: \"{e}\"", feeds_path.display()),
            )),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => logs.push((LogLevel::Warning, format!("Failed to read RSS session data. {e}"))),
    }

    for feed in tree.feeds_mut() {
        let hex = feed.uid.hex();
        let path = articles_dir.join(format!("{hex}.json"));
        match read_capped(&path, MAX_ARTICLES_FILE) {
            Ok(data) => {
                let (articles, warnings) = feed::load_articles(&data, &feed.url);
                logs.extend(warnings.into_iter().map(|w| (LogLevel::Warning, w)));
                feed.set_loaded_articles(articles, max);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => logs.push((LogLevel::Warning, format!("Failed to read RSS session data. {e}"))),
        }
        feed.icon_loaded = articles_dir.join(format!("{hex}.ico")).is_file();
    }

    let mut rules = Vec::new();
    match read_capped(&rss_dir.join(RULES_FILE), MAX_CONFIG_FILE) {
        Ok(data) => match rules_from_json(&data) {
            Ok(r) => rules = r,
            Err(e) => logs.push((LogLevel::Critical, format!("Couldn't load RSS AutoDownloader rules. Reason: {e}"))),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => logs.push((LogLevel::Warning, format!("Failed to read RSS AutoDownloader rules. {e}"))),
    }
    rules.retain(|r| !r.name.is_empty());
    rules.sort_by_key(|r| r.priority);
    Loaded { tree, rules, logs, resave_feeds }
}

impl RssManager {
    pub async fn launch(ctx: &CoreContext, session: Arc<Session>) -> QbtResult<Arc<Self>> {
        Self::launch_with(ctx, session).await
    }

    /// Starts the RSS session with a custom torrent adder.
    pub async fn launch_with(ctx: &CoreContext, adder: Arc<dyn TorrentAdder>) -> QbtResult<Arc<Self>> {
        let rss_dir = ctx.profile.rss_dir();
        let articles_dir = rss_dir.join(ARTICLES_DIR);
        let prefs = ctx.prefs.current().rss.clone();
        let max = effective_max(&prefs);
        let loaded = {
            let (rss_dir, articles_dir) = (rss_dir.clone(), articles_dir.clone());
            tokio::task::spawn_blocking(move || load_from_disk(&rss_dir, &articles_dir, max))
                .await
                .map_err(|e| QbtError::other(format!("RSS start-up failed: {e}")))?
        };
        let (store, writer) = Store::spawn(&ctx.rt, ctx.logger.clone());
        let match_ctx = MatchContext::new(&prefs.rss_smart_episode_filters, prefs.rss_download_repack_proper_episodes);
        let manager = Arc::new_cyclic(|weak| RssManager {
            ctx: ctx.clone(),
            adder,
            weak: weak.clone(),
            state: Mutex::new(State {
                tree: loaded.tree,
                rules: loaded.rules,
                match_ctx,
                prefs,
                refresh_deadline: None,
                queue: VecDeque::new(),
                queued: HashSet::new(),
                waiting: HashMap::new(),
                rules_dirty: false,
            }),
            store,
            hosts: fetch::HostQueue::default(),
            rss_dir,
            articles_dir,
            update_notify: Notify::new(),
            save_notify: Notify::new(),
            sched_notify: Notify::new(),
            job_notify: Notify::new(),
            fetches: Mutex::new(HashMap::new()),
            tasks: Mutex::new(vec![writer]),
            shut_down: AtomicBool::new(false),
        });

        for (level, msg) in loaded.logs {
            ctx.logger.log(level, msg);
        }
        manager.start_tasks();
        manager.with_state(|me, st, fx| {
            fx.save_feeds = loaded.resave_feeds;
            if st.prefs.rss_processing_enabled {
                me.session_refresh(st, fx);
            }
            if st.prefs.rss_auto_downloading_enabled {
                me.reset_processing_queue(st, fx);
            }
        });
        Ok(manager)
    }

    fn start_tasks(&self) {
        let Some(me) = self.weak.upgrade() else { return };
        let prefs_rx = self.ctx.prefs.subscribe();
        let rt = &self.ctx.rt;
        let handles = vec![
            rt.spawn(me.clone().scheduler_task(prefs_rx)),
            rt.spawn(me.clone().processor_task()),
            rt.spawn(me.clone().notifier_task()),
            rt.spawn(me.saver_task()),
        ];
        self.tasks.lock().extend(handles);
    }

    /// Stores pending changes and stops all background work.
    pub async fn shutdown(&self) {
        if self.shut_down.swap(true, Ordering::SeqCst) {
            return;
        }
        let tasks: Vec<_> = self.tasks.lock().drain(..).collect();
        let (writer, others) = tasks.split_first().map_or((None, &[][..]), |(w, o)| (Some(w), o));
        for t in others {
            t.abort();
        }
        for (_, h) in self.fetches.lock().drain() {
            h.abort();
        }
        self.flush_dirty();
        self.store.flush().await;
        if let Some(w) = writer {
            w.abort();
        }
    }

    fn feeds_path(&self) -> PathBuf {
        self.rss_dir.join(FEEDS_FILE)
    }

    fn rules_path(&self) -> PathBuf {
        self.rss_dir.join(RULES_FILE)
    }

    fn articles_path(&self, uid: Uid) -> PathBuf {
        self.articles_dir.join(format!("{}.json", uid.hex()))
    }

    fn icon_path(&self, uid: Uid) -> PathBuf {
        self.articles_dir.join(format!("{}.ico", uid.hex()))
    }

    fn live_prefs(&self) -> RssPrefs {
        self.ctx.prefs.current().rss.clone()
    }

    /// Runs `f` under the state lock, then performs the collected effects.
    fn with_state<T>(&self, f: impl FnOnce(&Self, &mut State, &mut Effects) -> T) -> T {
        let mut fx = Effects::default();
        let out = {
            let mut st = self.state.lock();
            let out = f(self, &mut st, &mut fx);
            self.prepare_writes(&mut st, &mut fx);
            out
        };
        self.apply(fx);
        out
    }

    fn prepare_writes(&self, st: &mut State, fx: &mut Effects) {
        if fx.save_feeds {
            fx.writes.push((self.feeds_path(), st.tree.feeds_file()));
        }
        let mut seen = HashSet::new();
        for uid in std::mem::take(&mut fx.save_articles) {
            if !seen.insert(uid) {
                continue;
            }
            if let Some(feed) = st.tree.feed_mut(uid)
                && feed.dirty
            {
                feed.dirty = false;
                fx.writes.push((self.articles_path(uid), feed.articles_file()));
            }
        }
        if fx.save_rules {
            st.rules_dirty = false;
            fx.writes.push((self.rules_path(), rules_file(&st.rules)));
        }
    }

    fn apply(&self, fx: Effects) {
        for (path, data) in fx.writes {
            self.store.write(path, data);
        }
        for path in fx.removes {
            self.store.remove(path);
        }
        for (level, msg) in fx.logs {
            self.ctx.logger.log(level, msg);
        }
        for uid in fx.aborts {
            if let Some(h) = self.fetches.lock().remove(&uid) {
                h.abort();
            }
        }
        let shutting_down = self.shut_down.load(Ordering::SeqCst);
        if !shutting_down {
            if let Some(me) = self.weak.upgrade() {
                for req in fx.fetches {
                    let uid = req.uid;
                    let handle = self.ctx.rt.spawn(me.clone().run_fetch(req));
                    if let Some(old) = self.fetches.lock().insert(uid, handle.abort_handle()) {
                        old.abort();
                    }
                }
                for add in fx.adds {
                    self.ctx.rt.spawn(me.clone().run_add(add));
                }
            }
            if fx.deferred_save {
                self.save_notify.notify_one();
            }
            if fx.reschedule {
                self.sched_notify.notify_one();
            }
            if fx.kick_jobs {
                self.job_notify.notify_one();
            }
        }
        if fx.changed {
            self.update_notify.notify_one();
        }
        for e in fx.events {
            self.ctx.events.emit(e);
        }
    }

    /// Writes every dirty feed and the rules if modified.
    fn flush_dirty(&self) {
        self.with_state(|_, st, fx| {
            fx.save_articles = st.tree.feeds().filter(|f| f.dirty).map(|f| f.uid).collect();
            fx.save_rules = st.rules_dirty;
        });
    }

    // ---- Background tasks -----------------------------------------------------

    async fn scheduler_task(self: Arc<Self>, prefs_rx: watch::Receiver<Arc<Preferences>>) {
        let mut prefs_rx = Some(prefs_rx);
        loop {
            let deadline = self.state.lock().refresh_deadline;
            let timer = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            let prefs_changed = async {
                match prefs_rx.as_mut() {
                    Some(rx) => rx.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = timer => {
                    self.with_state(|me, st, fx| {
                        st.refresh_deadline = None;
                        me.session_refresh(st, fx);
                    });
                }
                _ = self.sched_notify.notified() => {}
                ok = prefs_changed => {
                    if ok {
                        let p = prefs_rx.as_mut().map(|rx| rx.borrow_and_update().rss.clone());
                        if let Some(p) = p {
                            self.with_state(|me, st, fx| me.apply_prefs(st, fx, p));
                        }
                    } else {
                        prefs_rx = None;
                    }
                }
            }
        }
    }

    async fn processor_task(self: Arc<Self>) {
        loop {
            self.job_notify.notified().await;
            while self.with_state(|me, st, fx| me.process_next_job(st, fx)) {
                tokio::task::yield_now().await;
            }
        }
    }

    async fn notifier_task(self: Arc<Self>) {
        loop {
            self.update_notify.notified().await;
            tokio::time::sleep(UPDATE_DEBOUNCE).await;
            self.ctx.events.emit(CoreEvent::RssUpdated);
        }
    }

    async fn saver_task(self: Arc<Self>) {
        loop {
            self.save_notify.notified().await;
            tokio::time::sleep(DEFERRED_SAVE).await;
            self.flush_dirty();
        }
    }

    async fn run_fetch(self: Arc<Self>, req: FetchRequest) {
        let delay = Duration::from_secs(self.live_prefs().rss_fetch_delay.clamp(0, 86_400) as u64);
        let key = fetch::service_key(&req.url);
        let result = self.hosts.run(&key, delay, fetch::download_feed(&self.ctx, &req.url)).await;
        match result {
            Ok(downloaded) => {
                self.ctx.logger.normal(format!(
                    "RSS feed at '{}' is successfully downloaded. Starting to parse it.",
                    downloaded.url
                ));
                let last_build = self.state.lock().tree.feed(req.uid).map(|f| f.last_build_date.clone());
                if let Some(last_build) = last_build {
                    let now = date::now_ms();
                    let parsed = tokio::task::spawn_blocking(move || parser::parse(&downloaded.body, &last_build, now))
                        .await
                        .unwrap_or_else(|e| parser::ParseResult { error: e.to_string(), ..Default::default() });
                    self.with_state(|me, st, fx| me.handle_parsing_finished(st, fx, req.uid, req.generation, parsed, now));
                }
            }
            Err(reason) => self.with_state(|_, st, fx| {
                if let Some(feed) = st.tree.feed_mut(req.uid)
                    && feed.generation == req.generation
                {
                    feed.is_loading = false;
                    feed.has_error = true;
                    fx.warning(format!("Failed to download RSS feed at '{}'. Reason: {reason}", req.url));
                    fx.changed = true;
                }
            }),
        }
        if req.want_icon {
            self.download_icon(req.uid, &req.url, &key, delay).await;
        }
    }

    async fn download_icon(&self, uid: Uid, feed_url: &str, key: &str, delay: Duration) {
        let path = self.icon_path(uid);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return;
        }
        let Some(url) = fetch::icon_url(feed_url) else { return };
        let Ok(icon) = self.hosts.run(key, delay, fetch::download(&self.ctx, &url, fetch::MAX_ICON_SIZE)).await else {
            return;
        };
        if icon.body.is_empty() {
            return;
        }
        self.with_state(|_, st, fx| {
            if let Some(feed) = st.tree.feed_mut(uid) {
                feed.icon_loaded = true;
                fx.writes.push((path, icon.body));
                fx.changed = true;
            }
        });
    }

    async fn run_add(self: Arc<Self>, add: AddRequest) {
        let result = self.adder.add_torrent_url(add.url.clone(), add.params).await;
        self.with_state(|me, st, fx| {
            let job = if add.waiting { st.waiting.remove(&add.url) } else { None };
            match &result {
                Ok(_) => {
                    if let Some(job) = job {
                        me.mark_job_read(st, fx, &job);
                    }
                    fx.events.push(CoreEvent::RssTorrentDownloaded { rule: add.rule.clone(), article_title: add.title.clone() });
                }
                Err(QbtError::Conflict { .. }) => {
                    if let Some(job) = job {
                        me.mark_job_read(st, fx, &job);
                    }
                }
                Err(e) => fx.warning(format!("Failed to add torrent. Source: \"{}\". Reason: \"{e}\"", add.url)),
            }
        });
    }

    // ---- Session logic (under the lock) ---------------------------------------

    fn apply_prefs(&self, st: &mut State, fx: &mut Effects, new: RssPrefs) {
        let old = std::mem::replace(&mut st.prefs, new.clone());
        if old.rss_processing_enabled != new.rss_processing_enabled {
            if new.rss_processing_enabled {
                self.session_refresh(st, fx);
            } else {
                st.refresh_deadline = None;
                fx.reschedule = true;
            }
        } else if old.rss_refresh_interval != new.rss_refresh_interval && new.rss_processing_enabled {
            // The global timer restarts with the new full period. Feeds with
            // their own interval keep their schedule (upstream would delay
            // them until the restarted timer fires).
            let now = Instant::now();
            let restarted = now + global_interval(&new);
            let mut deadline = restarted;
            for feed in st.tree.feeds_mut() {
                if feed.refresh_interval == 0 {
                    if feed.next_refresh.is_some() {
                        feed.next_refresh = Some(restarted);
                    }
                } else if let Some(tp) = feed.next_refresh {
                    deadline = deadline.min(tp.max(now + Duration::from_secs(1)));
                }
            }
            st.refresh_deadline = Some(deadline);
            fx.reschedule = true;
        }
        if old.rss_max_articles_per_feed != new.rss_max_articles_per_feed {
            let max = effective_max(&new);
            for feed in st.tree.feeds_mut() {
                if feed.trim(max) {
                    fx.changed = true;
                }
            }
        }
        if old.rss_smart_episode_filters != new.rss_smart_episode_filters
            || old.rss_download_repack_proper_episodes != new.rss_download_repack_proper_episodes
        {
            st.match_ctx = MatchContext::new(&new.rss_smart_episode_filters, new.rss_download_repack_proper_episodes);
        }
        if old.rss_auto_downloading_enabled != new.rss_auto_downloading_enabled {
            if new.rss_auto_downloading_enabled {
                self.reset_processing_queue(st, fx);
            } else {
                st.queue.clear();
                st.queued.clear();
            }
        }
    }

    /// `Session::refresh`: refreshes due feeds and re-arms the timer.
    fn session_refresh(&self, st: &mut State, fx: &mut Effects) {
        if !self.live_prefs().rss_processing_enabled {
            return;
        }
        let prefs = self.live_prefs();
        let uids: Vec<Uid> = st.tree.feeds().map(|f| f.uid).collect();
        if uids.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut next: Option<Duration> = None;
        for uid in uids {
            let due = st.tree.feed(uid).is_some_and(|f| f.next_refresh.is_none_or(|tp| now + Duration::from_secs(1) > tp));
            if due {
                self.refresh_feed(st, fx, uid);
                if let Some(feed) = st.tree.feed_mut(uid) {
                    feed.next_refresh = Some(now + effective_interval(feed, &prefs));
                }
            }
            if let Some(tp) = st.tree.feed(uid).and_then(|f| f.next_refresh) {
                let interval = tp.saturating_duration_since(now);
                next = Some(next.map_or(interval, |n| n.min(interval)));
            }
        }
        st.refresh_deadline = Some(now + next.unwrap_or_default().max(Duration::from_secs(1)));
        fx.reschedule = true;
    }

    /// `Feed::refresh`: (re)starts the download.
    fn refresh_feed(&self, st: &mut State, fx: &mut Effects, uid: Uid) {
        let Some(feed) = st.tree.feed_mut(uid) else { return };
        feed.generation += 1;
        feed.is_loading = true;
        let want_icon = !feed.icon_attempted && !feed.icon_loaded;
        feed.icon_attempted = true;
        fx.fetches.push(FetchRequest { uid, url: feed.url.clone(), generation: feed.generation, want_icon });
        fx.changed = true;
    }

    fn handle_parsing_finished(
        &self,
        st: &mut State,
        fx: &mut Effects,
        uid: Uid,
        generation: u64,
        result: parser::ParseResult,
        now: i64,
    ) {
        let max = effective_max(&self.live_prefs());
        let Some(feed) = st.tree.feed_mut(uid) else { return };
        if feed.generation != generation {
            return;
        }
        feed.has_error = !result.error.is_empty();
        let mut title_changed = false;
        if !result.title.is_empty() && feed.title != result.title {
            feed.title = result.title;
            title_changed = true;
        }
        if !result.last_build_date.is_empty() {
            feed.last_build_date = result.last_build_date;
        }
        let (count, added) = feed.update_articles(result.articles, max, now);
        fx.save_articles.push(uid);
        if feed.has_error {
            fx.warning(format!("Failed to parse RSS feed at '{}'. Reason: {}", feed.url, result.error));
        }
        fx.normal(format!("RSS feed at '{}' updated. Added {count} new articles.", feed.url));
        feed.is_loading = false;
        fx.changed = true;

        if title_changed && feed.name() == feed.url && !feed.title.trim().is_empty() {
            // Now we have something better than a URL: try to rename the feed
            // (surrounding whitespace dropped; invalid names keep the URL).
            let (path, dest) = (feed.path.clone(), tree::join_path(tree::parent_path(&feed.path), feed.title.trim()));
            if st.tree.move_item(&path, &dest).is_ok() {
                fx.save_feeds = true;
            }
        }
        for id in added {
            self.handle_new_article(st, fx, uid, &id);
        }
    }

    /// `AutoDownloader::handleNewArticle`.
    fn handle_new_article(&self, st: &mut State, fx: &mut Effects, uid: Uid, id: &str) {
        if !st.prefs.rss_auto_downloading_enabled || !self.live_prefs().rss_auto_downloading_enabled {
            return;
        }
        let Some(feed) = st.tree.feed(uid) else { return };
        let Some(article) = feed.article(id) else { return };
        if article.is_read || article.torrent_url_or_link().is_empty() || st.waiting.contains_key(article.torrent_url_or_link()) {
            return;
        }
        if !st.queued.insert((uid, id.to_owned())) {
            return;
        }
        st.queue.push_back(Job {
            feed_uid: uid,
            feed_url: feed.url.clone(),
            article_id: article.id.clone(),
            title: article.title.clone(),
            date: article.date,
            torrent_url: article.torrent_url.clone(),
        });
        fx.kick_jobs = true;
    }

    /// `AutoDownloader::resetProcessingQueue`: re-enqueues every unread
    /// article (newest first across all feeds).
    fn reset_processing_queue(&self, st: &mut State, fx: &mut Effects) {
        st.queue.clear();
        st.queued.clear();
        if !self.live_prefs().rss_auto_downloading_enabled {
            return;
        }
        let mut all: Vec<(i64, Uid, String)> = Vec::new();
        for feed in st.tree.feeds() {
            for a in &feed.articles {
                if !a.is_read && !a.torrent_url_or_link().is_empty() {
                    all.push((a.date, feed.uid, a.id.clone()));
                }
            }
        }
        all.sort_by_key(|a| std::cmp::Reverse(a.0));
        for (_, uid, id) in all {
            self.handle_new_article(st, fx, uid, &id);
        }
    }

    /// `AutoDownloader::processJob` for the next queued job. Returns `true`
    /// while more jobs are queued.
    fn process_next_job(&self, st: &mut State, fx: &mut Effects) -> bool {
        let Some(job) = st.queue.pop_front() else { return false };
        st.queued.remove(&(job.feed_uid, job.article_id.clone()));
        if !self.live_prefs().rss_auto_downloading_enabled {
            st.queue.clear();
            st.queued.clear();
            return false;
        }
        let feed_url = st.tree.feed(job.feed_uid).map_or_else(|| job.feed_url.clone(), |f| f.url.clone());
        let State { rules, match_ctx, .. } = &mut *st;
        let mut accepted = None;
        for rule in rules.iter_mut() {
            if !rule.enabled || !rule.feed_urls.contains(&feed_url) {
                continue;
            }
            if rule.accepts(&job.title, job.date, match_ctx) {
                accepted = Some((rule.name.clone(), rule.add_torrent_params()));
                break;
            }
        }
        if let Some((rule_name, params)) = accepted {
            st.rules_dirty = true;
            fx.deferred_save = true;
            fx.normal(format!("RSS article '{}' is accepted by rule '{rule_name}'. Trying to add torrent...", job.title));
            let url = job.torrent_url.clone();
            if !is_supported_torrent_url(&url) {
                fx.warning(format!(
                    "Failed to add torrent from RSS article. Reason: unsupported torrent URL. Only HTTP(S) URLs, magnet URIs and info hashes are supported. Article: \"{}\". URL: \"{url}\"",
                    job.title
                ));
            } else {
                let descriptor = is_torrent_descriptor(&url);
                fx.adds.push(AddRequest { url: url.clone(), params, rule: rule_name, title: job.title.clone(), waiting: !descriptor });
                if descriptor {
                    self.mark_job_read(st, fx, &job);
                } else {
                    st.waiting.insert(url, job);
                }
            }
        }
        !st.queue.is_empty()
    }

    fn mark_job_read(&self, st: &mut State, fx: &mut Effects, job: &Job) {
        if let Some(feed) = st.tree.feed_mut(job.feed_uid)
            && feed.mark_read(&job.article_id)
        {
            fx.deferred_save = true;
            fx.changed = true;
        }
    }

    fn set_rule_impl(&self, st: &mut State, fx: &mut Effects, rule: Rule) {
        match st.rules.iter().position(|r| r.name == rule.name) {
            None => {
                st.rules.push(rule);
                fx.save_rules = true;
            }
            Some(i) => {
                if st.rules[i].to_json() == rule.to_json() && st.rules[i].name == rule.name {
                    return;
                }
                st.rules[i] = rule;
                st.rules_dirty = true;
                fx.deferred_save = true;
            }
        }
        st.rules.sort_by_key(|r| r.priority);
        self.reset_processing_queue(st, fx);
    }

    fn feed_uid_at(st: &State, path: &str) -> Option<Uid> {
        match st.tree.item(path) {
            Some(ItemRef::Feed(uid)) => Some(uid),
            _ => None,
        }
    }

    // ---- Public API -------------------------------------------------------------

    /// The whole tree; folders and feeds sorted by name.
    pub fn tree(&self, with_articles: bool) -> RssFolder {
        let st = self.state.lock();
        self.folder_dto(&st, "", with_articles)
    }

    fn folder_dto(&self, st: &State, path: &str, with_articles: bool) -> RssFolder {
        let mut folder = RssFolder { path: path.to_owned(), name: tree::relative_name(path).to_owned(), ..Default::default() };
        for name in st.tree.children(path) {
            let child = tree::join_path(path, &name);
            match st.tree.item(&child) {
                Some(ItemRef::Folder) => folder.folders.push(self.folder_dto(st, &child, with_articles)),
                Some(ItemRef::Feed(uid)) => {
                    if let Some(f) = st.tree.feed(uid) {
                        let icon = if f.icon_loaded { self.icon_path(uid).to_string_lossy().into_owned() } else { String::new() };
                        folder.feeds.push(f.to_dto(with_articles, icon));
                    }
                }
                None => {}
            }
        }
        folder
    }

    /// Articles of a feed, or of every feed below a folder, newest first.
    pub fn articles(&self, item_path: &str) -> QbtResult<Vec<RssArticle>> {
        let st = self.state.lock();
        if st.tree.item(item_path).is_none() {
            return Err(QbtError::not_found(format!("Item doesn't exist: {item_path}.")));
        }
        let mut out: Vec<RssArticle> = Vec::new();
        for uid in st.tree.feeds_under(item_path) {
            if let Some(f) = st.tree.feed(uid) {
                out.extend(f.articles.iter().map(feed::Article::to_dto));
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.date));
        Ok(out)
    }

    /// `rss/items` JSON: `{name: folder-object | feed-object}`.
    pub fn items_json(&self, with_data: bool) -> Value {
        let st = self.state.lock();
        st.tree.folder_json("", &|f| f.to_api_json(with_data))
    }

    pub fn add_folder(&self, path: &str) -> QbtResult<()> {
        self.with_state(|_, st, fx| {
            st.tree.add_folder(path).map_err(QbtError::conflict)?;
            fx.save_feeds = true;
            fx.changed = true;
            Ok(())
        })
    }

    /// Adds a feed; an empty `path` uses the URL as the name (WebAPI rule).
    pub fn add_feed(&self, url: &str, path: &str, refresh_interval_secs: i64) -> QbtResult<()> {
        let path = if path.is_empty() { url } else { path };
        self.with_state(|me, st, fx| {
            if st.tree.feed_by_url(url).is_some() {
                return Err(QbtError::conflict(format!("RSS feed with given URL already exists: {url}.")));
            }
            fetch::validate_feed_url(url).map_err(QbtError::conflict)?;
            let uid = st.tree.new_uid();
            st.tree.add_feed(uid, url, path, refresh_interval_secs.max(0)).map_err(QbtError::conflict)?;
            fx.save_feeds = true;
            fx.changed = true;
            me.session_refresh(st, fx);
            Ok(())
        })
    }

    pub fn set_feed_url(&self, path: &str, url: &str) -> QbtResult<()> {
        self.with_state(|me, st, fx| {
            let Some(uid) = Self::feed_uid_at(st, path) else {
                return Err(QbtError::conflict(format!("Feed doesn't exist: {path}.")));
            };
            let old_url = st.tree.feed(uid).map(|f| f.url.clone()).unwrap_or_default();
            if old_url == url {
                return Ok(());
            }
            fetch::validate_feed_url(url).map_err(QbtError::conflict)?;
            st.tree.set_feed_url(uid, url).map_err(QbtError::conflict)?;
            // A feed still named after its URL follows the new URL.
            if let Some(feed) = st.tree.feed(uid)
                && feed.name() == old_url
            {
                let (p, dest) = (feed.path.clone(), tree::join_path(tree::parent_path(&feed.path), url));
                let _ = st.tree.move_item(&p, &dest);
            }
            // Rules follow the feed (`handleFeedURLChanged`).
            for rule in &mut st.rules {
                for u in rule.feed_urls.iter_mut().filter(|u| **u == old_url) {
                    *u = url.to_owned();
                    fx.save_rules = true;
                }
            }
            for job in st.queue.iter_mut().chain(st.waiting.values_mut()) {
                if job.feed_url == old_url {
                    job.feed_url = url.to_owned();
                }
            }
            fx.save_feeds = true;
            fx.changed = true;
            let prefs = me.live_prefs();
            if prefs.rss_processing_enabled {
                me.refresh_feed(st, fx, uid);
                if let Some(feed) = st.tree.feed_mut(uid) {
                    feed.next_refresh = Some(Instant::now() + effective_interval(feed, &prefs));
                }
            }
            Ok(())
        })
    }

    pub fn set_feed_refresh_interval(&self, path: &str, secs: i64) -> QbtResult<()> {
        if secs < 0 {
            return Err(QbtError::invalid("Invalid 'refreshInterval' value"));
        }
        self.with_state(|me, st, fx| {
            let Some(uid) = Self::feed_uid_at(st, path) else {
                return Err(QbtError::conflict(format!("Feed doesn't exist: {path}.")));
            };
            let Some(feed) = st.tree.feed_mut(uid) else { return Ok(()) };
            let old = feed.refresh_interval;
            if old == secs {
                return Ok(());
            }
            feed.refresh_interval = secs;
            if let Some(tp) = feed.next_refresh {
                feed.next_refresh = Some(if secs >= old {
                    tp + Duration::from_secs((secs - old) as u64)
                } else {
                    tp.checked_sub(Duration::from_secs((old - secs) as u64)).unwrap_or_else(Instant::now)
                });
            }
            fx.save_feeds = true;
            fx.changed = true;
            let prefs = me.live_prefs();
            if prefs.rss_processing_enabled {
                let old_effective = if old > 0 { old } else { global_interval(&prefs).as_secs() as i64 };
                if secs < old_effective {
                    me.session_refresh(st, fx);
                }
            }
            Ok(())
        })
    }

    pub fn remove_item(&self, path: &str) -> QbtResult<()> {
        self.with_state(|me, st, fx| {
            let removed = st.tree.remove_item(path).map_err(QbtError::conflict)?;
            for feed in removed {
                fx.aborts.push(feed.uid);
                fx.removes.push(me.articles_path(feed.uid));
                fx.removes.push(me.icon_path(feed.uid));
                st.queue.retain(|j| j.feed_uid != feed.uid);
                st.queued.retain(|(u, _)| *u != feed.uid);
                st.waiting.retain(|_, j| j.feed_uid != feed.uid);
            }
            fx.save_feeds = true;
            fx.changed = true;
            Ok(())
        })
    }

    /// Moves or renames an item.
    pub fn move_item(&self, item: &str, dest: &str) -> QbtResult<()> {
        self.with_state(|_, st, fx| {
            st.tree.move_item(item, dest).map_err(QbtError::conflict)?;
            fx.save_feeds = true;
            fx.changed = true;
            Ok(())
        })
    }

    /// Marks one article (by id) of a feed, or a whole feed/folder, as read.
    /// Unknown items are ignored (WebAPI behaviour).
    pub fn mark_as_read(&self, item: &str, article_id: Option<&str>) -> QbtResult<()> {
        self.with_state(|_, st, fx| {
            match (st.tree.item(item), article_id) {
                (None, _) => {}
                (Some(ItemRef::Feed(uid)), Some(id)) => {
                    if let Some(feed) = st.tree.feed_mut(uid)
                        && feed.mark_read(id)
                    {
                        fx.deferred_save = true;
                        fx.changed = true;
                    }
                }
                (Some(ItemRef::Folder), Some(_)) => {}
                (Some(_), None) => {
                    for uid in st.tree.feeds_under(item) {
                        if let Some(feed) = st.tree.feed_mut(uid)
                            && feed.mark_all_read()
                        {
                            fx.save_articles.push(uid);
                            fx.changed = true;
                        }
                    }
                }
            }
            Ok(())
        })
    }

    /// Refreshes a feed or every feed below a folder (`""` = all). Unknown
    /// items are ignored. Works even when automatic fetching is disabled.
    pub fn refresh_item(&self, item: &str) -> QbtResult<()> {
        self.with_state(|me, st, fx| {
            for uid in st.tree.feeds_under(item) {
                me.refresh_feed(st, fx, uid);
            }
            Ok(())
        })
    }

    /// Rules in evaluation order (ascending priority).
    pub fn rules(&self) -> Vec<RssAutoDownloadRule> {
        self.state.lock().rules.iter().map(Rule::to_dto).collect()
    }

    /// `rss/rules` JSON: `{name: rule-object}`.
    pub fn rules_json(&self) -> Value {
        let st = self.state.lock();
        Value::Object(st.rules.iter().map(|r| (r.name.clone(), r.to_json())).collect())
    }

    /// Creates or replaces the rule named `rule.name`.
    pub fn set_rule(&self, rule: RssAutoDownloadRule) -> QbtResult<()> {
        validate_rule_name(&rule.name)?;
        self.with_state(|me, st, fx| {
            let previous = st.rules.iter().find(|r| r.name == rule.name);
            let new = Rule::from_dto(&rule, previous);
            me.set_rule_impl(st, fx, new);
            Ok(())
        })
    }

    /// WebAPI `rss/setRule`: `rule_def` is the rule JSON object; invalid JSON
    /// gives a rule with default values (qBittorrent behaviour).
    pub fn set_rule_json(&self, name: &str, rule_def: &str) -> QbtResult<()> {
        validate_rule_name(name)?;
        let obj = match serde_json::from_str::<Value>(rule_def) {
            Ok(Value::Object(o)) => o,
            _ => Map::new(),
        };
        let rule = Rule::from_json(&obj, name);
        self.with_state(|me, st, fx| me.set_rule_impl(st, fx, rule));
        Ok(())
    }

    /// Renames a rule. `NotFound` if it doesn't exist, `Conflict` if the new
    /// name is taken (the WebAPI ignores both, like qBittorrent).
    pub fn rename_rule(&self, old: &str, new: &str) -> QbtResult<()> {
        validate_rule_name(new)?;
        self.with_state(|_, st, fx| {
            if st.rules.iter().any(|r| r.name == new) {
                return Err(QbtError::conflict("A rule with this name already exists, please choose another name."));
            }
            let Some(rule) = st.rules.iter_mut().find(|r| r.name == old) else {
                return Err(QbtError::not_found(format!("Rule doesn't exist: {old}.")));
            };
            rule.name = new.to_owned();
            fx.save_rules = true;
            Ok(())
        })
    }

    /// Removes a rule (missing rules are ignored).
    pub fn remove_rule(&self, name: &str) -> QbtResult<()> {
        self.with_state(|_, st, fx| {
            let before = st.rules.len();
            st.rules.retain(|r| r.name != name);
            if st.rules.len() != before {
                fx.save_rules = true;
            }
            Ok(())
        })
    }

    /// `rss/matchingArticles`: per affected feed, the titles of stored
    /// articles the rule matches. Unknown rules match nothing.
    pub fn matching_articles(&self, rule_name: &str) -> QbtResult<Vec<RssMatchingArticles>> {
        let st = self.state.lock();
        let Some(rule) = st.rules.iter().find(|r| r.name == rule_name) else {
            return Ok(Vec::new());
        };
        Ok(Self::matching_for(&st, rule))
    }

    /// Like [`Self::matching_articles`] for an unsaved rule (RSS Downloader
    /// dialog preview while editing).
    pub fn matching_articles_for(&self, rule: &RssAutoDownloadRule) -> Vec<RssMatchingArticles> {
        let st = self.state.lock();
        let previous = st.rules.iter().find(|r| r.name == rule.name);
        let rule = Rule::from_dto(rule, previous);
        Self::matching_for(&st, &rule)
    }

    fn matching_for(st: &State, rule: &Rule) -> Vec<RssMatchingArticles> {
        let mut out = Vec::new();
        for url in &rule.feed_urls {
            let Some(feed) = st.tree.feed_by_url(url) else { continue };
            let titles: Vec<String> = feed
                .articles
                .iter()
                .filter(|a| rule.matches(&a.title, a.date, &st.match_ctx).is_some())
                .map(|a| a.title.clone())
                .collect();
            if !titles.is_empty() {
                out.push(RssMatchingArticles { feed_name: feed.name().to_owned(), article_titles: titles });
            }
        }
        out
    }

    pub fn clear_rule_history(&self, rule_name: &str) -> QbtResult<()> {
        self.with_state(|me, st, fx| {
            let Some(rule) = st.rules.iter_mut().find(|r| r.name == rule_name) else {
                return Err(QbtError::not_found(format!("Rule doesn't exist: {rule_name}.")));
            };
            if !rule.previously_matched.is_empty() {
                rule.previously_matched.clear();
                st.rules_dirty = true;
                fx.deferred_save = true;
                me.reset_processing_queue(st, fx);
            }
            Ok(())
        })
    }

    /// Imports qBittorrent rules: JSON (`*.json`) or the legacy
    /// `QDataStream` format (`*.rssrules`). Same-named rules are replaced.
    pub fn import_rules(&self, data: &[u8]) -> QbtResult<()> {
        if data.len() as u64 > MAX_CONFIG_FILE {
            return Err(QbtError::invalid(format!("File size ({}) exceeds limit ({MAX_CONFIG_FILE})", data.len())));
        }
        let is_json = data.iter().find(|b| !b.is_ascii_whitespace()).is_some_and(|b| *b == b'{');
        let rules = if is_json { rules_from_json(data) } else { legacy::import(data) }.map_err(QbtError::invalid)?;
        self.with_state(|me, st, fx| {
            for rule in rules {
                if validate_rule_name(&rule.name).is_ok() {
                    me.set_rule_impl(st, fx, rule);
                }
            }
            fx.save_rules = true;
        });
        Ok(())
    }

    /// Exports every rule as qBittorrent JSON (pretty-printed).
    pub fn export_rules(&self) -> Vec<u8> {
        rules_file(&self.state.lock().rules)
    }

    /// Exports every rule in the legacy `*.rssrules` format.
    pub fn export_rules_legacy(&self) -> Vec<u8> {
        legacy::export(&self.state.lock().rules)
    }
}

fn validate_rule_name(name: &str) -> QbtResult<()> {
    if name.is_empty() || name.len() > MAX_RULE_NAME || name.chars().any(char::is_control) {
        return Err(QbtError::invalid("Invalid rule name."));
    }
    Ok(())
}

fn rules_file(rules: &[Rule]) -> Vec<u8> {
    let obj: Map<String, Value> = rules.iter().map(|r| (r.name.clone(), r.to_json())).collect();
    tree::to_pretty_json(&Value::Object(obj))
}

/// Validates a Must Contain / Must Not Contain expression for the editor:
/// empty string if valid, else a description of the problem.
pub fn validate_rule_expression(expression: &str, use_regex: bool) -> String {
    if use_regex {
        if expression.len() > 4096 {
            return "Expression is too long".to_owned();
        }
        return match regex::RegexBuilder::new(expression).size_limit(2 * 1024 * 1024).build() {
            Ok(_) => String::new(),
            Err(e) => e.to_string(),
        };
    }
    for alternative in expression.split('|') {
        for token in alternative.split_whitespace() {
            if rule::wildcard_to_regex(token).and_then(|p| rule::compile_user_regex(&p, false)).is_none() {
                return format!("Invalid wildcard: {token}");
            }
        }
    }
    String::new()
}
