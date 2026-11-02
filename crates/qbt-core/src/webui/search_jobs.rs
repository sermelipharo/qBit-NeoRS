//! WebUI search jobs (qBittorrent 5.3 `SearchJobManager`): the jobs started
//! through `search/start`, shared by every WebUI session, and their storage
//! across restarts.
//!
//! With `store_search_jobs` on, the jobs (ID, pattern, category, plugins) are
//! kept in `<data>/WebUI/Search/Session.json`; with `store_search_job_results`
//! also on, each finished job's results go to `<data>/WebUI/Search/NNNN.json`
//! (the ID, at least four digits). Restored jobs are stopped. Turning the
//! options off deletes what they stored.
//!
//! A running job lives in the search engine; once it stops, its results are
//! moved here and the engine's job is deleted. The stored data is read on the
//! first `search/*` request rather than at start-up, so a profile that never
//! uses the WebUI does not hold restored results in memory.
//!
//! Bounds (beyond qBittorrent's): at most [`MAX_JOBS`] jobs (the oldest
//! stopped one is dropped), stored patterns, categories and plugin lists
//! within the engine's limits, results per job within the engine's cap, and
//! a results file never larger than [`RESULTS_FILE_MAX_SIZE`] (the results
//! that do not fit are not stored).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};

use crate::app::App;
use crate::error::{QbtError, QbtResult};
use crate::model::{SearchJobInfo, SearchJobStatus, SearchResult, SearchResultsPage};
use crate::search::{MAX_RESULTS_PER_JOB, is_valid_plugin_name, page_range};
use crate::utils::fs::write_atomic;

/// Folder inside the profile's data folder (qBittorrent `DATA_FOLDER`).
pub const DATA_FOLDER: &str = "WebUI/Search";
const SESSION_FILE_NAME: &str = "Session.json";
pub const SESSION_FILE_MAX_SIZE: usize = 1024 * 1024;
pub const RESULTS_FILE_MAX_SIZE: usize = 10 * 1024 * 1024;
pub const MAX_CONCURRENT_SEARCHES: usize = 5;
/// Jobs kept (qBittorrent has no limit).
pub const MAX_JOBS: usize = 50;
const MAX_PATTERN_CHARS: usize = 1024;
const MAX_PLUGINS: usize = 256;
const MAX_CATEGORY_LEN: usize = 64;

const KEY_JOBS: &str = "Jobs";
const KEY_JOB_ID: &str = "ID";
const KEY_JOB_SEARCH_PATTERN: &str = "SearchPattern";
const KEY_JOB_CATEGORY: &str = "Category";
const KEY_JOB_PLUGINS: &str = "Plugins";

const KEY_RESULT_FILE_NAME: &str = "FileName";
const KEY_RESULT_FILE_URL: &str = "FileURL";
const KEY_RESULT_FILE_SIZE: &str = "FileSize";
const KEY_RESULT_SEEDERS_COUNT: &str = "SeedersCount";
const KEY_RESULT_LEECHERS_COUNT: &str = "LeechersCount";
const KEY_RESULT_ENGINE_NAME: &str = "EngineName";
const KEY_RESULT_SITE_URL: &str = "SiteURL";
const KEY_RESULT_DESCR_LINK: &str = "DescrLink";
const KEY_RESULT_PUB_DATE: &str = "PubDate";

/// The search engine as the job manager uses it (the facade, or a fake in
/// tests).
pub(crate) trait SearchBackend {
    fn start(&self, pattern: &str, plugins: &[String], category: &str) -> QbtResult<i64>;
    fn info(&self, id: i64) -> QbtResult<SearchJobInfo>;
    fn page(&self, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage>;
    fn stop(&self, id: i64) -> QbtResult<()>;
    fn delete(&self, id: i64) -> QbtResult<()>;
}

impl SearchBackend for App {
    fn start(&self, pattern: &str, plugins: &[String], category: &str) -> QbtResult<i64> {
        self.search_start(pattern.to_owned(), plugins.to_vec(), category.to_owned())
    }

    fn info(&self, id: i64) -> QbtResult<SearchJobInfo> {
        self.search_status(Some(id))?.into_iter().next().ok_or_else(|| QbtError::not_found(format!("search job {id}")))
    }

    fn page(&self, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
        self.search_results(id, limit, offset)
    }

    fn stop(&self, id: i64) -> QbtResult<()> {
        self.search_stop(id)
    }

    fn delete(&self, id: i64) -> QbtResult<()> {
        self.search_delete(id)
    }
}

/// What `search/status` reports for a job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobInfo {
    pub id: i64,
    pub pattern: String,
    pub category: String,
    pub plugins: Vec<String>,
    pub active: bool,
    pub total: usize,
}

struct Job {
    id: i64,
    pattern: String,
    category: String,
    plugins: Vec<String>,
    /// The search engine's job while it runs.
    engine_id: Option<i64>,
    /// Result count of the running job (last seen).
    running_total: usize,
    /// Results once stopped (or restored).
    results: Vec<SearchResult>,
}

impl Job {
    fn active(&self) -> bool {
        self.engine_id.is_some()
    }

    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.id,
            pattern: self.pattern.clone(),
            category: self.category.clone(),
            plugins: self.plugins.clone(),
            active: self.active(),
            total: if self.active() { self.running_total } else { self.results.len() },
        }
    }
}

struct State {
    loaded: bool,
    store_jobs: bool,
    store_results: bool,
    /// Oldest first.
    jobs: Vec<Job>,
}

pub struct SearchJobManager {
    dir: PathBuf,
    warn: Box<dyn Fn(String) + Send + Sync>,
    state: Mutex<State>,
}

impl SearchJobManager {
    /// `data_dir`: the profile's data folder. Nothing is read until the
    /// first request.
    pub fn new(data_dir: &Path, store_jobs: bool, store_results: bool, warn: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            dir: data_dir.join(DATA_FOLDER),
            warn: Box::new(warn),
            state: Mutex::new(State { loaded: false, store_jobs, store_results, jobs: Vec::new() }),
        }
    }

    /// Locks the state, loads the stored session once and brings the running
    /// jobs up to date.
    fn with_state<R>(&self, b: &dyn SearchBackend, f: impl FnOnce(&Self, &mut State) -> R) -> R {
        let mut st = self.state.lock();
        if !st.loaded {
            st.loaded = true;
            self.load_session(&mut st);
        }
        let (store_jobs, store_results) = (st.store_jobs, st.store_results);
        for job in st.jobs.iter_mut() {
            self.refresh(b, job, store_jobs && store_results);
        }
        f(self, &mut st)
    }

    /// Follows the engine's job; once it stopped, takes its results (saving
    /// them when asked to) and deletes it from the engine.
    fn refresh(&self, b: &dyn SearchBackend, job: &mut Job, save_results: bool) {
        let Some(engine_id) = job.engine_id else { return };
        match b.info(engine_id) {
            Ok(info) if info.status == SearchJobStatus::Running => {
                job.running_total = usize::try_from(info.total).unwrap_or(0);
            }
            Ok(_) => {
                job.results = b.page(engine_id, 0, 0).map(|p| p.results).unwrap_or_default();
                job.results.truncate(MAX_RESULTS_PER_JOB);
                let _ = b.delete(engine_id);
                job.engine_id = None;
                if save_results {
                    self.save_search_results(job);
                }
            }
            // Gone from the engine (it drops old jobs when it has too many).
            Err(_) => job.engine_id = None,
        }
    }

    /// `search/start`. `plugins` are the plugin names to use ("all" /
    /// "enabled" already expanded).
    pub fn start_search(&self, b: &dyn SearchBackend, pattern: &str, category: &str, plugins: &[String]) -> QbtResult<i64> {
        self.with_state(b, |me, st| {
            if st.jobs.iter().filter(|j| j.active()).count() >= MAX_CONCURRENT_SEARCHES {
                return Err(QbtError::conflict(format!("Unable to create more than {MAX_CONCURRENT_SEARCHES} concurrent searches.")));
            }
            let engine_id = b.start(pattern, plugins, category)?;
            while st.jobs.len() >= MAX_JOBS {
                let Some(i) = st.jobs.iter().position(|j| !j.active()) else { break };
                let old = st.jobs.remove(i);
                me.remove_search_results(old.id);
            }
            let id = loop {
                let id = i64::from(rand::random_range(1..=i32::MAX));
                if !st.jobs.iter().any(|j| j.id == id) {
                    break id;
                }
            };
            // The engine accepted them, so they are within its limits.
            let plugins = plugins.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).map(str::to_owned).collect();
            st.jobs.push(Job {
                id,
                pattern: pattern.to_owned(),
                category: category.to_owned(),
                plugins,
                engine_id: Some(engine_id),
                running_total: 0,
                results: Vec::new(),
            });
            me.save_session(st);
            Ok(id)
        })
    }

    /// `search/stop`: false when there is no such job.
    pub fn stop_search(&self, b: &dyn SearchBackend, id: i64) -> bool {
        self.with_state(b, |_, st| {
            let Some(job) = st.jobs.iter().find(|j| j.id == id) else { return false };
            if let Some(engine_id) = job.engine_id {
                let _ = b.stop(engine_id);
            }
            true
        })
    }

    /// `search/delete`: stops the job and removes it with its stored results.
    pub fn delete_search(&self, b: &dyn SearchBackend, id: i64) -> bool {
        self.with_state(b, |me, st| {
            let Some(i) = st.jobs.iter().position(|j| j.id == id) else { return false };
            let job = st.jobs.remove(i);
            if let Some(engine_id) = job.engine_id {
                let _ = b.delete(engine_id);
            }
            me.remove_search_results(id);
            me.save_session(st);
            true
        })
    }

    pub fn job_info(&self, b: &dyn SearchBackend, id: i64) -> Option<JobInfo> {
        self.with_state(b, |_, st| st.jobs.iter().find(|j| j.id == id).map(Job::info))
    }

    /// Every job, oldest first.
    pub fn all_job_infos(&self, b: &dyn SearchBackend) -> Vec<JobInfo> {
        self.with_state(b, |_, st| st.jobs.iter().map(Job::info).collect())
    }

    /// `search/results`: `limit <= 0` = all; a negative `offset` counts from
    /// the end.
    pub fn results(&self, b: &dyn SearchBackend, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
        self.with_state(b, |_, st| {
            let job = st.jobs.iter().find(|j| j.id == id).ok_or_else(|| QbtError::not_found(format!("search job {id}")))?;
            if let Some(engine_id) = job.engine_id {
                return b.page(engine_id, limit, offset);
            }
            let range = page_range(job.results.len(), limit, offset)?;
            Ok(SearchResultsPage {
                status: SearchJobStatus::Stopped,
                total: job.results.len() as i64,
                results: job.results[range].to_vec(),
            })
        })
    }

    /// A search engine job changed (its results are kept as soon as it stops).
    pub fn engine_job_updated(&self, b: &dyn SearchBackend, engine_id: i64) {
        let mut st = self.state.lock();
        let save_results = st.store_jobs && st.store_results;
        if let Some(job) = st.jobs.iter_mut().find(|j| j.engine_id == Some(engine_id)) {
            self.refresh(b, job, save_results);
        }
    }

    /// Brings every running job up to date (missed engine events).
    pub fn refresh_all(&self, b: &dyn SearchBackend) {
        let mut st = self.state.lock();
        let save_results = st.store_jobs && st.store_results;
        for job in st.jobs.iter_mut() {
            self.refresh(b, job, save_results);
        }
    }

    /// qBittorrent's `onPreferencesChanged`.
    pub fn preferences_changed(&self, store_jobs: bool, store_results: bool) {
        let mut st = self.state.lock();
        if store_jobs != st.store_jobs {
            st.store_jobs = store_jobs;
            self.save_session(&st);
        }
        if store_results != st.store_results {
            st.store_results = store_results;
            if st.store_results && st.store_jobs {
                for job in st.jobs.iter().filter(|j| !j.active()) {
                    self.save_search_results(job);
                }
            } else {
                self.remove_all_result_files();
            }
        }
    }

    // ---- Storage ---------------------------------------------------------------

    fn session_path(&self) -> PathBuf {
        self.dir.join(SESSION_FILE_NAME)
    }

    fn results_path(&self, id: i64) -> PathBuf {
        self.dir.join(format!("{id:04}.json"))
    }

    fn load_session(&self, st: &mut State) {
        if !st.store_jobs {
            return;
        }
        let path = self.session_path();
        let data = match read_file(&path, SESSION_FILE_MAX_SIZE) {
            Ok(Some(d)) => d,
            Ok(None) => return,
            Err(e) => {
                (self.warn)(format!("Failed to load WebUI search session. File: \"{}\". Error: \"{e}\"", path.display()));
                return;
            }
        };
        let doc: Value = match serde_json::from_slice(&data) {
            Ok(v) => v,
            Err(e) => {
                (self.warn)(format!("Failed to parse WebUI search session. File: \"{}\". Error: \"{e}\"", path.display()));
                return;
            }
        };
        let jobs = doc.get(KEY_JOBS).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        for value in jobs {
            if st.jobs.len() >= MAX_JOBS {
                break;
            }
            let Some(job) = parse_job(value) else { continue };
            if st.jobs.iter().any(|j| j.id == job.id) {
                continue;
            }
            let mut job = job;
            if st.store_results {
                job.results = self.load_search_results(job.id);
            }
            st.jobs.push(job);
        }
    }

    fn save_session(&self, st: &State) {
        if !st.store_jobs {
            self.remove_all_data();
            return;
        }
        let mut jobs: Vec<Value> = st
            .jobs
            .iter()
            .map(|j| {
                json!({
                    KEY_JOB_ID: j.id,
                    KEY_JOB_SEARCH_PATTERN: j.pattern,
                    KEY_JOB_CATEGORY: j.category,
                    KEY_JOB_PLUGINS: j.plugins,
                })
            })
            .collect();
        let path = self.session_path();
        // The oldest jobs are left out if the file would be too large to load.
        let data = loop {
            let data = serde_json::to_vec(&json!({ KEY_JOBS: jobs })).unwrap_or_default();
            if data.len() <= SESSION_FILE_MAX_SIZE || jobs.is_empty() {
                break data;
            }
            jobs.remove(0);
        };
        if let Err(e) = write_atomic(&path, &data) {
            (self.warn)(format!("Failed to save WebUI search session. File: \"{}\". Error: \"{e}\"", path.display()));
        }
    }

    fn load_search_results(&self, id: i64) -> Vec<SearchResult> {
        let path = self.results_path(id);
        let parsed = read_file(&path, RESULTS_FILE_MAX_SIZE).and_then(|data| match data {
            None => Ok(Vec::new()),
            Some(d) => parse_results(&d),
        });
        parsed.unwrap_or_else(|e| {
            (self.warn)(format!("Failed to load WebUI search results. File: \"{}\". Error: \"{e}\"", path.display()));
            Vec::new()
        })
    }

    fn save_search_results(&self, job: &Job) {
        let path = self.results_path(job.id);
        if let Err(e) = write_atomic(&path, &serialize_results(&job.results, RESULTS_FILE_MAX_SIZE)) {
            (self.warn)(format!("Failed to save WebUI search results. File: \"{}\". Error: \"{e}\"", path.display()));
        }
    }

    fn remove_search_results(&self, id: i64) {
        let _ = std::fs::remove_file(self.results_path(id));
    }

    fn remove_all_data(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }

    /// Removes the results files (only `<digits>.json` regular files).
    fn remove_all_result_files(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let is_results = name.strip_suffix(".json").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if is_results && entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Reads a whole file of at most `max` bytes; `None` when it doesn't exist.
fn read_file(path: &Path, max: usize) -> Result<Option<Vec<u8>>, String> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > max as u64 {
        return Err(format!("File size exceeds limit. File size: {size}. Size limit: {max}"));
    }
    let mut data = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.take(max as u64 + 1).read_to_end(&mut data).map_err(|e| e.to_string())?;
    if data.len() > max {
        return Err(format!("File size exceeds limit. Size limit: {max}"));
    }
    Ok(Some(data))
}

/// `QJsonValue::toInteger`.
fn to_integer(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)).unwrap_or(0),
        _ => 0,
    }
}

fn to_string(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// One stored job; `None` when it is invalid (qBittorrent skips IDs <= 0 and
/// empty patterns; values beyond the engine's limits are refused too).
fn parse_job(v: &Value) -> Option<Job> {
    let o = v.as_object()?;
    let id = o.get(KEY_JOB_ID).and_then(Value::as_i64).filter(|id| (1..=i64::from(i32::MAX)).contains(id))?;
    let pattern = to_string(o.get(KEY_JOB_SEARCH_PATTERN));
    if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN_CHARS {
        return None;
    }
    let category = to_string(o.get(KEY_JOB_CATEGORY));
    if category.len() > MAX_CATEGORY_LEN {
        return None;
    }
    let plugins: Vec<String> = o
        .get(KEY_JOB_PLUGINS)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_str)
        .filter(|p| is_valid_plugin_name(p))
        .take(MAX_PLUGINS)
        .map(str::to_owned)
        .collect();
    Some(Job { id, pattern, category, plugins, engine_id: None, running_total: 0, results: Vec::new() })
}

fn parse_results(data: &[u8]) -> Result<Vec<SearchResult>, String> {
    let doc: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let Value::Array(items) = doc else { return Err("Invalid data format".into()) };
    Ok(items
        .iter()
        .filter_map(Value::as_object)
        .take(MAX_RESULTS_PER_JOB)
        .map(|o| SearchResult {
            file_name: to_string(o.get(KEY_RESULT_FILE_NAME)),
            file_url: to_string(o.get(KEY_RESULT_FILE_URL)),
            file_size: to_integer(o.get(KEY_RESULT_FILE_SIZE)),
            seeders: to_integer(o.get(KEY_RESULT_SEEDERS_COUNT)),
            leechers: to_integer(o.get(KEY_RESULT_LEECHERS_COUNT)),
            engine_name: to_string(o.get(KEY_RESULT_ENGINE_NAME)),
            site_url: to_string(o.get(KEY_RESULT_SITE_URL)),
            descr_link: to_string(o.get(KEY_RESULT_DESCR_LINK)),
            pub_date: to_integer(o.get(KEY_RESULT_PUB_DATE)),
        })
        .collect())
}

/// A JSON array of the results that fit in `max` bytes.
fn serialize_results(results: &[SearchResult], max: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(results.len().saturating_mul(256).min(max));
    out.push(b'[');
    for r in results {
        let mut o = Map::with_capacity(9);
        o.insert(KEY_RESULT_FILE_NAME.into(), json!(r.file_name));
        o.insert(KEY_RESULT_FILE_URL.into(), json!(r.file_url));
        o.insert(KEY_RESULT_FILE_SIZE.into(), json!(r.file_size));
        o.insert(KEY_RESULT_SEEDERS_COUNT.into(), json!(r.seeders));
        o.insert(KEY_RESULT_LEECHERS_COUNT.into(), json!(r.leechers));
        o.insert(KEY_RESULT_ENGINE_NAME.into(), json!(r.engine_name));
        o.insert(KEY_RESULT_SITE_URL.into(), json!(r.site_url));
        o.insert(KEY_RESULT_DESCR_LINK.into(), json!(r.descr_link));
        o.insert(KEY_RESULT_PUB_DATE.into(), json!(r.pub_date));
        let item = serde_json::to_vec(&Value::Object(o)).unwrap_or_default();
        // Separator, item and the closing bracket must fit.
        if out.len() + usize::from(out.len() > 1) + item.len() + 1 > max {
            break;
        }
        if out.len() > 1 {
            out.push(b',');
        }
        out.extend_from_slice(&item);
    }
    out.push(b']');
    out
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;

    /// An in-memory search engine.
    #[derive(Default)]
    struct FakeEngine {
        next: Mutex<i64>,
        jobs: Mutex<HashMap<i64, (bool, Vec<SearchResult>)>>,
        started: Mutex<Vec<(String, Vec<String>, String)>>,
    }

    impl FakeEngine {
        fn finish(&self, id: i64, results: Vec<SearchResult>) {
            self.jobs.lock().insert(id, (false, results));
        }
    }

    impl SearchBackend for FakeEngine {
        fn start(&self, pattern: &str, plugins: &[String], category: &str) -> QbtResult<i64> {
            let mut n = self.next.lock();
            *n += 1;
            self.jobs.lock().insert(*n, (true, Vec::new()));
            self.started.lock().push((pattern.into(), plugins.to_vec(), category.into()));
            Ok(*n)
        }
        fn info(&self, id: i64) -> QbtResult<SearchJobInfo> {
            let jobs = self.jobs.lock();
            let (running, results) = jobs.get(&id).ok_or_else(|| QbtError::not_found("job"))?;
            Ok(SearchJobInfo {
                id,
                status: if *running { SearchJobStatus::Running } else { SearchJobStatus::Stopped },
                total: results.len() as i64,
                ..Default::default()
            })
        }
        fn page(&self, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
            let jobs = self.jobs.lock();
            let (running, results) = jobs.get(&id).ok_or_else(|| QbtError::not_found("job"))?;
            let range = page_range(results.len(), limit, offset)?;
            Ok(SearchResultsPage {
                status: if *running { SearchJobStatus::Running } else { SearchJobStatus::Stopped },
                total: results.len() as i64,
                results: results[range].to_vec(),
            })
        }
        fn stop(&self, id: i64) -> QbtResult<()> {
            if let Some(j) = self.jobs.lock().get_mut(&id) {
                j.0 = false;
            }
            Ok(())
        }
        fn delete(&self, id: i64) -> QbtResult<()> {
            self.jobs.lock().remove(&id).map(|_| ()).ok_or_else(|| QbtError::not_found("job"))
        }
    }

    fn result(n: usize) -> SearchResult {
        SearchResult {
            file_name: format!("file {n}"),
            file_url: format!("magnet:?xt=urn:btih:{n:040}"),
            file_size: 1000 + n as i64,
            seeders: 5,
            leechers: 2,
            engine_name: "engine".into(),
            site_url: "https://example.org".into(),
            descr_link: format!("https://example.org/{n}"),
            pub_date: 1_700_000_000,
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        warnings: Arc<Mutex<Vec<String>>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self { dir: tempfile::tempdir().unwrap(), warnings: Arc::default() }
        }

        fn manager(&self, store_jobs: bool, store_results: bool) -> SearchJobManager {
            let w = self.warnings.clone();
            SearchJobManager::new(self.dir.path(), store_jobs, store_results, move |m| w.lock().push(m))
        }

        fn folder(&self) -> PathBuf {
            self.dir.path().join(DATA_FOLDER)
        }

        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.folder())
                .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            names.sort();
            names
        }
    }

    fn plugins(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn round_trip_restores_stopped_jobs_with_results() {
        let f = Fixture::new();
        let engine = FakeEngine::default();
        let m = f.manager(true, true);
        let a = m.start_search(&engine, "ubuntu iso", "software", &plugins(&["piratebay", "eztv"])).unwrap();
        let b = m.start_search(&engine, "debian", "all", &plugins(&["piratebay"])).unwrap();
        assert_eq!(engine.started.lock()[0], ("ubuntu iso".into(), plugins(&["piratebay", "eztv"]), "software".into()));
        let info = m.job_info(&engine, a).unwrap();
        assert!(info.active);
        assert_eq!((info.pattern.as_str(), info.category.as_str()), ("ubuntu iso", "software"));
        assert_eq!(info.plugins, plugins(&["piratebay", "eztv"]));

        // The first job finishes: its results move here and are stored.
        engine.finish(1, (0..3).map(result).collect());
        m.engine_job_updated(&engine, 1);
        assert!(engine.jobs.lock().get(&1).is_none(), "the engine's job is deleted");
        assert_eq!(f.files(), vec![format!("{a:04}.json"), "Session.json".to_owned()]);
        let page = m.results(&engine, a, 2, -2).unwrap();
        assert_eq!((page.status, page.total), (SearchJobStatus::Stopped, 3));
        assert_eq!(page.results, vec![result(1), result(2)]);

        // A restart: both jobs come back, stopped, in order; results for the finished one.
        let m2 = f.manager(true, true);
        let all = m2.all_job_infos(&FakeEngine::default());
        assert_eq!(all.iter().map(|j| j.id).collect::<Vec<_>>(), vec![a, b]);
        assert!(all.iter().all(|j| !j.active));
        assert_eq!(all[0].total, 3);
        assert_eq!(all[0].plugins, plugins(&["piratebay", "eztv"]));
        assert_eq!((all[1].pattern.as_str(), all[1].category.as_str(), all[1].total), ("debian", "all", 0));
        assert_eq!(m2.results(&FakeEngine::default(), a, 0, 0).unwrap().results, (0..3).map(result).collect::<Vec<_>>());
        assert!(f.warnings.lock().is_empty(), "{:?}", f.warnings.lock());
    }

    #[test]
    fn delete_removes_files_and_stop_reaches_the_engine() {
        let f = Fixture::new();
        let engine = FakeEngine::default();
        let m = f.manager(true, true);
        let a = m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap();
        assert!(m.stop_search(&engine, a));
        m.refresh_all(&engine);
        assert!(!m.job_info(&engine, a).unwrap().active);
        assert!(f.folder().join(format!("{a:04}.json")).exists());
        assert!(m.delete_search(&engine, a));
        assert!(!m.delete_search(&engine, a));
        assert!(!m.stop_search(&engine, a));
        assert_eq!(f.files(), vec!["Session.json".to_owned()]);
        let text = std::fs::read_to_string(f.folder().join("Session.json")).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({"Jobs": []}));
        // Deleting a running job deletes the engine's job too.
        let b = m.start_search(&engine, "b", "all", &plugins(&["x"])).unwrap();
        assert!(m.delete_search(&engine, b));
        assert!(engine.jobs.lock().is_empty());
    }

    #[test]
    fn nothing_is_stored_when_off_and_turning_off_deletes() {
        let f = Fixture::new();
        let engine = FakeEngine::default();
        let m = f.manager(false, false);
        let a = m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap();
        engine.finish(1, vec![result(1)]);
        m.refresh_all(&engine);
        assert!(!f.folder().exists());

        // Turning the options on stores the session and the finished results.
        m.preferences_changed(true, false);
        assert_eq!(f.files(), vec!["Session.json".to_owned()]);
        m.preferences_changed(true, true);
        assert_eq!(f.files(), vec![format!("{a:04}.json"), "Session.json".to_owned()]);
        // Results off deletes only results files (never other files).
        std::fs::write(f.folder().join("notes.json"), "{}").unwrap();
        m.preferences_changed(true, false);
        assert_eq!(f.files(), vec!["Session.json".to_owned(), "notes.json".to_owned()]);
        // Jobs off deletes the folder.
        m.preferences_changed(false, false);
        assert!(!f.folder().exists());
    }

    #[test]
    fn restored_results_need_the_results_option() {
        let f = Fixture::new();
        let engine = FakeEngine::default();
        let m = f.manager(true, true);
        let a = m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap();
        engine.finish(1, vec![result(1)]);
        m.refresh_all(&engine);
        let restored = f.manager(true, false).all_job_infos(&engine);
        assert_eq!((restored[0].id, restored[0].total), (a, 0));
        assert!(f.manager(false, true).all_job_infos(&engine).is_empty());
    }

    #[test]
    fn corrupt_and_oversized_files_are_ignored_with_a_warning() {
        let f = Fixture::new();
        std::fs::create_dir_all(f.folder()).unwrap();
        let session = f.folder().join("Session.json");

        std::fs::write(&session, b"{not json").unwrap();
        assert!(f.manager(true, true).all_job_infos(&FakeEngine::default()).is_empty());
        assert!(f.warnings.lock().pop().unwrap().starts_with("Failed to parse WebUI search session. File: "));

        std::fs::write(&session, vec![b' '; SESSION_FILE_MAX_SIZE + 1]).unwrap();
        assert!(f.manager(true, true).all_job_infos(&FakeEngine::default()).is_empty());
        let w = f.warnings.lock().pop().unwrap();
        assert!(w.starts_with("Failed to load WebUI search session. File: ") && w.contains("exceeds limit"), "{w}");

        // Invalid jobs are skipped; a corrupt results file loses only its results.
        let jobs = json!({"Jobs": [
            {"ID": 0, "SearchPattern": "zero id"},
            {"ID": 7, "SearchPattern": ""},
            {"ID": 3_000_000_000_i64, "SearchPattern": "too big"},
            {"ID": 8, "SearchPattern": "ok", "Category": "movies", "Plugins": ["a", 5, "../x", "b"]},
            {"ID": 8, "SearchPattern": "duplicate"},
            {"ID": 9, "SearchPattern": "corrupt results"},
            {"ID": 10, "SearchPattern": "oversized results"},
            "not an object",
        ]});
        std::fs::write(&session, jobs.to_string()).unwrap();
        std::fs::write(f.folder().join("0009.json"), b"[{").unwrap();
        std::fs::write(f.folder().join("0010.json"), vec![b' '; RESULTS_FILE_MAX_SIZE + 1]).unwrap();
        let all = f.manager(true, true).all_job_infos(&FakeEngine::default());
        assert_eq!(all.iter().map(|j| j.id).collect::<Vec<_>>(), vec![8, 9, 10]);
        assert_eq!(all[0].plugins, plugins(&["a", "b"]));
        assert_eq!((all[0].category.as_str(), all[0].total), ("movies", 0));
        let w = f.warnings.lock().clone();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|m| m.starts_with("Failed to load WebUI search results. File: ")));
    }

    #[test]
    fn concurrency_and_job_limits() {
        let f = Fixture::new();
        let engine = FakeEngine::default();
        let m = f.manager(false, false);
        for _ in 0..MAX_CONCURRENT_SEARCHES {
            m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap();
        }
        let e = m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap_err();
        assert_eq!(e, QbtError::conflict("Unable to create more than 5 concurrent searches."));
        // Stopped jobs don't count; beyond MAX_JOBS the oldest stopped job goes.
        for _ in 0..MAX_JOBS {
            for id in engine.jobs.lock().values_mut() {
                id.0 = false;
            }
            m.start_search(&engine, "a", "all", &plugins(&["x"])).unwrap();
        }
        assert_eq!(m.all_job_infos(&engine).len(), MAX_JOBS);
    }

    #[test]
    fn results_files_stay_within_the_limit() {
        let results: Vec<SearchResult> = (0..100).map(result).collect();
        let full = serialize_results(&results, usize::MAX);
        assert_eq!(parse_results(&full).unwrap(), results);
        let cut = serialize_results(&results, full.len() / 2);
        assert!(cut.len() <= full.len() / 2);
        let kept = parse_results(&cut).unwrap();
        assert!(!kept.is_empty() && kept.len() < 100);
        assert_eq!(kept[..], results[..kept.len()]);
        assert_eq!(serialize_results(&results, 2), b"[]");
    }
}
