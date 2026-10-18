//! Execution log: in-memory ring buffers for the GUI/WebAPI ("General" and
//! "Blocked IPs"), optional stderr echo and the rotating file log configured
//! on the Behavior page (`file_log_*`).

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::app::Profile;
use crate::events::{CoreEvent, EventBus};
use crate::model::{LogLevel, LogMessage, PeerLogEntry};
use crate::prefs::{BehaviorPrefs, FileLogAgeType, PreferencesStore};

/// Same cap as qBittorrent's `MAX_LOG_MESSAGES`.
const MAX_LOG_MESSAGES: usize = 20_000;
const LOG_FILE_NAME: &str = "qbittorrent.log";

pub struct Logger {
    inner: Mutex<Inner>,
    events: EventBus,
    prefs: Arc<PreferencesStore>,
    default_dir: PathBuf,
    stderr: bool,
}

struct Inner {
    messages: VecDeque<LogMessage>,
    peers: VecDeque<PeerLogEntry>,
    next_id: i64,
    next_peer_id: i64,
    file: Option<OpenLog>,
}

struct OpenLog {
    path: PathBuf,
    file: File,
    size: u64,
}

impl Logger {
    pub fn new(profile: &Profile, prefs: &Arc<PreferencesStore>, events: EventBus, stderr: bool) -> Self {
        Self {
            inner: Mutex::new(Inner {
                messages: VecDeque::new(),
                peers: VecDeque::new(),
                next_id: 0,
                next_peer_id: 0,
                file: None,
            }),
            events,
            prefs: prefs.clone(),
            default_dir: profile.logs_dir(),
            stderr,
        }
    }

    pub fn normal(&self, msg: impl Into<String>) {
        self.log(LogLevel::Normal, msg)
    }
    pub fn info(&self, msg: impl Into<String>) {
        self.log(LogLevel::Info, msg)
    }
    pub fn warning(&self, msg: impl Into<String>) {
        self.log(LogLevel::Warning, msg)
    }
    pub fn critical(&self, msg: impl Into<String>) {
        self.log(LogLevel::Critical, msg)
    }

    pub fn log(&self, level: LogLevel, msg: impl Into<String>) {
        let message = msg.into();
        let timestamp = chrono::Utc::now().timestamp_millis();
        let entry = {
            let mut g = self.inner.lock();
            let entry = LogMessage { id: g.next_id, timestamp, level, message };
            g.next_id += 1;
            if g.messages.len() >= MAX_LOG_MESSAGES {
                g.messages.pop_front();
            }
            g.messages.push_back(entry.clone());
            self.write_file(&mut g, &entry);
            entry
        };
        if self.stderr {
            eprintln!("{}", format_line(&entry));
        }
        self.events.emit(CoreEvent::Log { message: entry });
    }

    /// Records a peer connection that was blocked (IP filter, ban) or banned.
    pub fn peer(&self, ip: impl Into<String>, blocked: bool, reason: impl Into<String>) {
        let mut g = self.inner.lock();
        let entry = PeerLogEntry {
            id: g.next_peer_id,
            ip: ip.into(),
            timestamp: chrono::Utc::now().timestamp_millis(),
            blocked,
            reason: reason.into(),
        };
        g.next_peer_id += 1;
        if g.peers.len() >= MAX_LOG_MESSAGES {
            g.peers.pop_front();
        }
        g.peers.push_back(entry);
    }

    pub fn messages(&self, last_known_id: i64, levels: &[LogLevel]) -> Vec<LogMessage> {
        let g = self.inner.lock();
        g.messages
            .iter()
            .filter(|m| m.id > last_known_id && (levels.is_empty() || levels.contains(&m.level)))
            .cloned()
            .collect()
    }

    pub fn peers(&self, last_known_id: i64) -> Vec<PeerLogEntry> {
        let g = self.inner.lock();
        g.peers.iter().filter(|m| m.id > last_known_id).cloned().collect()
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock();
        g.messages.clear();
        g.peers.clear();
    }

    pub fn flush(&self) {
        let mut g = self.inner.lock();
        if let Some(f) = g.file.as_mut() {
            let _ = f.file.flush();
        }
    }

    fn write_file(&self, g: &mut Inner, entry: &LogMessage) {
        let prefs = self.prefs.current();
        let b = &prefs.behavior;
        if !b.file_log_enabled {
            g.file = None;
            return;
        }
        let dir = if b.file_log_path.is_empty() { self.default_dir.clone() } else { PathBuf::from(&b.file_log_path) };
        let path = dir.join(LOG_FILE_NAME);
        if g.file.as_ref().map(|f| f.path != path).unwrap_or(true) {
            g.file = open_log(&path);
        }
        let Some(open) = g.file.as_mut() else { return };
        let line = format!("{}\n", format_line(entry));
        if open.file.write_all(line.as_bytes()).is_ok() {
            open.size += line.len() as u64;
        }
        let max = (b.file_log_max_size.max(1) as u64) * 1024;
        if b.file_log_backup_enabled && open.size >= max {
            drop(g.file.take());
            rotate(&path, b);
            g.file = open_log(&path);
        }
    }
}

fn open_log(path: &Path) -> Option<OpenLog> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path).ok()?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Some(OpenLog { path: path.to_path_buf(), file, size })
}

/// `qbittorrent.log` -> `qbittorrent.log.bak`, `.bak1`, ... (first free name),
/// then deletes backups older than the configured age.
fn rotate(path: &Path, b: &BehaviorPrefs) {
    let mut n = 0;
    let backup = loop {
        let candidate = if n == 0 {
            PathBuf::from(format!("{}.bak", path.display()))
        } else {
            PathBuf::from(format!("{}.bak{n}", path.display()))
        };
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
    };
    let _ = std::fs::rename(path, &backup);
    if b.file_log_delete_old {
        delete_old_backups(path, b);
    }
}

fn delete_old_backups(path: &Path, b: &BehaviorPrefs) {
    let days = match b.file_log_age_type {
        FileLogAgeType::Days => b.file_log_age,
        FileLogAgeType::Months => b.file_log_age * 30,
        FileLogAgeType::Years => b.file_log_age * 365,
    }
    .max(1) as u64;
    let max_age = std::time::Duration::from_secs(days * 86_400);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let prefix = format!("{name}.bak");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let fname = e.file_name();
        if !fname.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|age| age > max_age)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// qBittorrent's file log line format: `(N) 2025-01-31T12:34:56 - message`.
fn format_line(m: &LogMessage) -> String {
    let tag = match m.level {
        LogLevel::Normal => 'N',
        LogLevel::Info => 'I',
        LogLevel::Warning => 'W',
        LogLevel::Critical => 'C',
    };
    let ts = chrono::DateTime::from_timestamp_millis(m.timestamp)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%dT%H:%M:%S").to_string())
        .unwrap_or_default();
    format!("({tag}) {ts} - {}", m.message)
}
