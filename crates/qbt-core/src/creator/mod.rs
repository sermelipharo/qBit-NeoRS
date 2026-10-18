//! Torrent creator (qBittorrent `base/bittorrent/torrentcreator`,
//! `torrentcreationmanager`, `torrentcreationtask`): v1, v2 and hybrid
//! torrents, automatic piece size, file alignment / padding files, private
//! flag, trackers with tiers, web seeds, comment, source; tasks run one at a
//! time on a blocking thread with progress and can start seeding when done.
//!
//! Finished .torrent files are written atomically (temporary file + rename)
//! to `torrent_file_path`, or to `<profile>/cache/torrent_creator/<id>.torrent`
//! (mode 0600) when no path is given; small results are also kept in memory
//! (bounded budget) so `torrent_file` normally does no disk I/O.

mod bencode;
pub mod builder;
pub mod layout;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;

use crate::bittorrent::Session;
use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};
use crate::events::CoreEvent;
use crate::model::*;

/// `TorrentCreator/Manager/MaxTasks`.
const MAX_TASKS: usize = 256;
/// Results up to this size are cached in memory ...
const MAX_CACHED_TORRENT: usize = 1024 * 1024;
/// ... within this total budget.
const CACHE_BUDGET: usize = 16 * 1024 * 1024;

struct Task {
    seq: u64,
    info: CreatorTask,
    params: TorrentCreatorParams,
    cancel: Arc<AtomicBool>,
    /// The finished .torrent on disk.
    file: Option<PathBuf>,
    /// `file` is our temporary file (deleted with the task).
    temp_file: bool,
    bytes: Option<Arc<Vec<u8>>>,
}

impl Task {
    fn is_finished(&self) -> bool {
        matches!(self.info.status, CreatorTaskStatus::Finished | CreatorTaskStatus::Failed)
    }
}

#[derive(Default)]
struct State {
    tasks: Vec<Task>,
    next_seq: u64,
    pump_running: bool,
    cached_bytes: usize,
}

impl State {
    fn find(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.info.id == id)
    }

    /// Removes a task, returning its temporary file (if any) for deletion.
    fn remove(&mut self, id: &str) -> Option<Task> {
        let pos = self.tasks.iter().position(|t| t.info.id == id)?;
        let task = self.tasks.remove(pos);
        if let Some(b) = &task.bytes {
            self.cached_bytes -= b.len();
        }
        task.cancel.store(true, Ordering::Relaxed);
        Some(task)
    }
}

pub struct CreatorManager {
    ctx: CoreContext,
    session: Arc<Session>,
    this: Weak<CreatorManager>,
    state: Mutex<State>,
    temp_dir: PathBuf,
}

impl CreatorManager {
    pub async fn launch(ctx: &CoreContext, session: Arc<Session>) -> QbtResult<Arc<Self>> {
        let temp_dir = ctx.profile.cache_dir.join("torrent_creator");
        // Results of a previous run are unreachable (task ids are not
        // persisted, like qBittorrent): start from an empty folder.
        let _ = std::fs::remove_dir_all(&temp_dir);
        Ok(Arc::new_cyclic(|this| Self {
            ctx: ctx.clone(),
            session,
            this: this.clone(),
            state: Mutex::new(State::default()),
            temp_dir,
        }))
    }

    pub fn add_task(&self, params: TorrentCreatorParams) -> QbtResult<String> {
        if params.source_path.is_empty() || !Path::new(&params.source_path).is_absolute() {
            return Err(QbtError::invalid("source path must be an absolute path"));
        }
        if !params.torrent_file_path.is_empty() {
            let p = Path::new(&params.torrent_file_path);
            let is_torrent = p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("torrent"));
            // The output may only be a .torrent file: never let the creator
            // overwrite arbitrary files (shell profiles, authorized_keys ...).
            if !p.is_absolute() || !is_torrent || p.file_name().is_none() {
                return Err(QbtError::invalid("torrent file path must be an absolute path ending with .torrent"));
            }
        }

        let now = chrono::Utc::now().timestamp();
        let id;
        let mut removed = Vec::new();
        {
            let mut st = self.state.lock();
            if st.tasks.len() >= MAX_TASKS {
                // Evict finished tasks, oldest first.
                let mut finished: Vec<(i64, u64, String)> = st
                    .tasks
                    .iter()
                    .filter(|t| t.is_finished())
                    .map(|t| (t.info.time_added, t.seq, t.info.id.clone()))
                    .collect();
                finished.sort();
                for (_, _, tid) in finished {
                    if st.tasks.len() < MAX_TASKS {
                        break;
                    }
                    if let Some(t) = st.remove(&tid) {
                        removed.push(t);
                    }
                }
            }
            if st.tasks.len() >= MAX_TASKS {
                return Err(QbtError::conflict("Too many active tasks"));
            }
            id = loop {
                let candidate = new_task_id();
                if st.find(&candidate).is_none() {
                    break candidate;
                }
            };
            let seq = st.next_seq;
            st.next_seq += 1;
            let info = CreatorTask {
                id: id.clone(),
                source_path: params.source_path.clone(),
                status: CreatorTaskStatus::Queued,
                progress: 0.0,
                error_message: String::new(),
                torrent_file_path: params.torrent_file_path.clone(),
                piece_size: params.piece_size,
                format: params.format,
                time_added: now,
                time_started: 0,
                time_finished: 0,
                info_hash: String::new(),
            };
            st.tasks.push(Task {
                seq,
                info,
                params,
                cancel: Arc::new(AtomicBool::new(false)),
                file: None,
                temp_file: false,
                bytes: None,
            });
        }
        self.discard_files(removed);
        self.emit(&id);
        self.kick();
        Ok(id)
    }

    /// All tasks: unfinished first, then finished, each in order of addition.
    pub fn tasks(&self) -> Vec<CreatorTask> {
        let st = self.state.lock();
        let mut v: Vec<&Task> = st.tasks.iter().collect();
        v.sort_by_key(|t| (t.is_finished(), t.info.time_added, t.seq));
        v.into_iter().map(|t| t.info.clone()).collect()
    }

    pub fn task(&self, id: &str) -> QbtResult<CreatorTask> {
        self.state.lock().find(id).map(|t| t.info.clone()).ok_or_else(|| task_not_found(id))
    }

    /// The created .torrent (`torrentcreator/torrentFile`).
    pub fn torrent_file(&self, id: &str) -> QbtResult<Vec<u8>> {
        let (path, limit) = {
            let mut st = self.state.lock();
            let task = st.find(id).ok_or_else(|| task_not_found(id))?;
            match task.info.status {
                CreatorTaskStatus::Queued | CreatorTaskStatus::Running => {
                    return Err(QbtError::conflict("Torrent creation is still unfinished."));
                }
                CreatorTaskStatus::Failed => return Err(QbtError::conflict("Torrent creation failed.")),
                CreatorTaskStatus::Finished => {}
            }
            if let Some(b) = &task.bytes {
                return Ok(b.as_ref().clone());
            }
            let path = task.file.clone().ok_or_else(|| QbtError::conflict("Torrent creation failed."))?;
            (path, self.ctx.prefs.current().advanced.torrent_file_size_limit.max(0) as u64)
        };
        read_limited(&path, limit).map_err(QbtError::conflict)
    }

    /// Removes a task; a running creation is interrupted.
    pub fn delete_task(&self, id: &str) -> QbtResult<()> {
        let removed = self.state.lock().remove(id).ok_or_else(|| task_not_found(id))?;
        self.discard_files(vec![removed]);
        self.emit(id);
        Ok(())
    }

    /// "Calculate number of pieces" in the creator dialog.
    pub fn calculate_total_pieces(source: &Path, piece_size: i64, format: TorrentFormat) -> QbtResult<i64> {
        let cancel = AtomicBool::new(false);
        let c = layout::collect_files(source, &cancel).map_err(QbtError::invalid)?;
        let total: u64 = c.files.iter().map(|f| f.size).sum();
        if total == 0 {
            return Ok(0);
        }
        let piece = builder::resolve_piece_size(piece_size, total, format).map_err(QbtError::invalid)?;
        let entries = match format {
            TorrentFormat::V1 => layout::plain_layout(&c.files),
            _ => layout::canonical_layout(&c.files, piece),
        };
        let size: u64 = entries.iter().map(|e| e.size).sum();
        Ok(size.div_ceil(piece) as i64)
    }

    // ---- internals ------------------------------------------------------------

    fn emit(&self, id: &str) {
        self.ctx.events.emit(CoreEvent::CreatorTaskUpdated { id: id.to_string() });
    }

    fn discard_files(&self, tasks: Vec<Task>) {
        let files: Vec<PathBuf> = tasks.into_iter().filter(|t| t.temp_file).filter_map(|t| t.file).collect();
        if !files.is_empty() {
            self.ctx.rt.spawn_blocking(move || {
                for f in files {
                    let _ = std::fs::remove_file(f);
                }
            });
        }
    }

    /// Starts the worker loop unless it is already running.
    fn kick(&self) {
        {
            let mut st = self.state.lock();
            if st.pump_running || !st.tasks.iter().any(|t| t.info.status == CreatorTaskStatus::Queued) {
                return;
            }
            st.pump_running = true;
        }
        if let Some(this) = self.this.upgrade() {
            self.ctx.rt.spawn(async move { this.pump().await });
        }
    }

    async fn pump(self: Arc<Self>) {
        loop {
            let job = {
                let mut st = self.state.lock();
                let next = st
                    .tasks
                    .iter_mut()
                    .filter(|t| t.info.status == CreatorTaskStatus::Queued)
                    .min_by_key(|t| t.seq);
                match next {
                    Some(t) => {
                        t.info.status = CreatorTaskStatus::Running;
                        t.info.time_started = chrono::Utc::now().timestamp();
                        t.info.progress = 0.0;
                        (t.info.id.clone(), t.params.clone(), t.cancel.clone())
                    }
                    None => {
                        st.pump_running = false;
                        return;
                    }
                }
            };
            let (id, params, cancel) = job;
            self.emit(&id);

            let worker = self.clone();
            let job_id = id.clone();
            let job_params = params.clone();
            let outcome = tokio::task::spawn_blocking(move || worker.run_job(&job_id, &job_params, &cancel))
                .await
                .unwrap_or_else(|e| Err(format!("internal error: {e}")));
            self.finish_job(&id, &params, outcome).await;
        }
    }

    /// Runs on a blocking thread: hashes, builds and writes the torrent.
    fn run_job(&self, id: &str, p: &TorrentCreatorParams, cancel: &AtomicBool) -> Result<JobOutput, String> {
        let created_by = format!("qBit NeoRS v{}", env!("CARGO_PKG_VERSION"));
        let max_torrent_size = match self.ctx.prefs.current().advanced.torrent_file_size_limit {
            n if n > 0 => n as u64,
            _ => 100 * 1024 * 1024,
        };
        let build = builder::BuildParams {
            source: Path::new(&p.source_path),
            format: p.format,
            piece_size: p.piece_size,
            optimize_alignment: p.optimize_alignment,
            padding_file_limit: p.padding_file_limit,
            private: p.private,
            trackers: &p.trackers,
            url_seeds: &p.url_seeds,
            comment: &p.comment,
            source_tag: &p.source,
            created_by: &created_by,
            creation_date: chrono::Utc::now().timestamp(),
            max_torrent_size,
        };
        let mut last_step = -1i64;
        let mut on_progress = |pct: f64| {
            let step = pct.floor() as i64;
            if step != last_step {
                last_step = step;
                self.set_progress(id, step as f64);
            }
        };
        let result = builder::build_torrent(&build, cancel, &mut on_progress)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(layout::ABORTED.to_string());
        }

        let (path, temp) = if p.torrent_file_path.is_empty() {
            (self.temp_dir.join(format!("{id}.torrent")), true)
        } else {
            (PathBuf::from(&p.torrent_file_path), false)
        };
        let mode = if temp { 0o600 } else { 0o644 };
        crate::utils::fs::write_atomic_mode(&path, &result.data, mode).map_err(|e| e.to_string())?;
        Ok(JobOutput { result, path, temp })
    }

    fn set_progress(&self, id: &str, progress: f64) {
        let changed = {
            let mut st = self.state.lock();
            match st.find(id) {
                Some(t) if t.info.progress != progress => {
                    t.info.progress = progress;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit(id);
        }
    }

    async fn finish_job(&self, id: &str, params: &TorrentCreatorParams, outcome: Result<JobOutput, String>) {
        let now = chrono::Utc::now().timestamp();
        let out = match outcome {
            Ok(out) => out,
            Err(reason) => {
                {
                    let mut st = self.state.lock();
                    if let Some(t) = st.find(id) {
                        t.info.status = CreatorTaskStatus::Failed;
                        t.info.error_message =
                            format!("Create new torrent file failed. Reason: {}.", reason.trim_end_matches('.'));
                        t.info.time_finished = now;
                    }
                }
                self.emit(id);
                return;
            }
        };

        if !out.result.skipped_links.is_empty() {
            let shown: Vec<&str> = out.result.skipped_links.iter().take(10).map(String::as_str).collect();
            self.ctx.logger.warning(format!(
                "Torrent creator skipped {} symbolic link(s) pointing to folders or outside the source folder: {}{}",
                out.result.skipped_links.len(),
                shown.join(", "),
                if out.result.skipped_links.len() > shown.len() { ", ..." } else { "" }
            ));
        }

        // Record the result; a task deleted meanwhile just drops its file.
        let still_exists = {
            let mut st = self.state.lock();
            let cache = out.result.data.len() <= MAX_CACHED_TORRENT
                && st.cached_bytes + out.result.data.len() <= CACHE_BUDGET;
            let mut cached = 0;
            let exists = match st.find(id) {
                Some(t) => {
                    t.info.info_hash = out.result.info_hash_hex();
                    t.info.piece_size = out.result.piece_size as i64;
                    t.info.progress = 100.0;
                    t.info.time_finished = now;
                    t.file = Some(out.path.clone());
                    t.temp_file = out.temp;
                    if cache {
                        cached = out.result.data.len();
                        t.bytes = Some(Arc::new(out.result.data.clone()));
                    }
                    true
                }
                None => false,
            };
            st.cached_bytes += cached;
            exists
        };
        if !still_exists {
            if out.temp {
                let _ = tokio::fs::remove_file(&out.path).await;
            }
            return;
        }

        let mut error = String::new();
        if params.start_seeding {
            let parent = Path::new(&params.source_path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let unlimited = params.ignore_share_limits;
            let add = AddTorrentParams {
                save_path: Some(parent),
                skip_checking: true,
                stopped: Some(false),
                content_layout: Some(ContentLayout::Original),
                stop_condition: Some(StopCondition::None),
                auto_tmm: Some(false),
                use_download_path: Some(false),
                ratio_limit: unlimited.then_some(-1.0),
                seeding_time_limit: unlimited.then_some(-1),
                inactive_seeding_time_limit: unlimited.then_some(-1),
                ..Default::default()
            };
            if let Err(e) = self.session.add_torrent(TorrentSource::Bytes { data: out.result.data }, add).await {
                self.ctx.logger.warning(format!(
                    "Failed to start seeding the created torrent. Torrent: \"{}\". Reason: {e}",
                    out.result.name
                ));
                error = "Failed to start seeding.".to_string();
            }
        }
        {
            let mut st = self.state.lock();
            if let Some(t) = st.find(id) {
                if error.is_empty() {
                    t.info.status = CreatorTaskStatus::Finished;
                } else {
                    t.info.status = CreatorTaskStatus::Failed;
                    t.info.error_message = error;
                }
            }
        }
        self.emit(id);
    }
}

struct JobOutput {
    result: builder::BuildResult,
    path: PathBuf,
    temp: bool,
}

fn task_not_found(id: &str) -> QbtError {
    QbtError::not_found(format!("torrent creator task {id}"))
}

/// Random (v4) UUID without braces, like `QUuid::toString(WithoutBraces)`.
fn new_task_id() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = builder::hex(&b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut data = Vec::new();
    let cap = if limit == 0 { u64::MAX } else { limit };
    file.take(cap.saturating_add(1)).read_to_end(&mut data).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() as u64 > cap {
        return Err(format!("File size exceeds limit. File: \"{}\". Limit: {cap}", path.display()));
    }
    Ok(data)
}

/// Automatic piece size (libtorrent heuristic; the same for every format).
pub fn auto_piece_size(total_size: i64, _format: TorrentFormat) -> i64 {
    builder::auto_piece_size(total_size.max(0) as u64) as i64
}

#[cfg(test)]
mod tests;
