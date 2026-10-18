//! One running search (`SearchHandler`): a `nova2.py` process whose stdout
//! is parsed line by line into results, with timeout, cancellation and
//! bounded memory.

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::AsyncReadExt;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::process::{GroupGuard, python_command, read_capped, terminate};
use super::proxy::ProxyEnv;
use crate::error::{QbtError, QbtResult};
use crate::events::{CoreEvent, EventBus};
use crate::logger::Logger;
use crate::model::{SearchJobInfo, SearchJobOutcome, SearchJobStatus, SearchResult, SearchResultsPage};

/// Results kept per job; further results are dropped.
pub(crate) const MAX_RESULTS_PER_JOB: usize = 50_000;
/// Approximate memory budget per job for result strings.
pub(crate) const MAX_JOB_BYTES: usize = 32 * 1024 * 1024;
/// Longest accepted output line; longer lines are discarded.
pub(crate) const MAX_LINE_LEN: usize = 16 * 1024;
/// qBittorrent's single-shot search timer.
pub(crate) const SEARCH_TIMEOUT: Duration = Duration::from_secs(180);
/// Minimum delay between two `SearchUpdated` events of one job.
const EMIT_INTERVAL: Duration = Duration::from_millis(300);
const STDERR_CAP: usize = 64 * 1024;
/// Stderr excerpt length written to the log / job error.
const ERROR_TEXT_CAP: usize = 4096;

/// Parses one `nova2.py` output line:
/// `link|name|size|seeds|leech|engine_url[|desc_link[|pub_date]]`.
/// Lines with fewer than 6 fields are rejected; extra fields are ignored.
pub(crate) fn parse_result_line(line: &[u8], engine_of: impl Fn(&str) -> String) -> Option<SearchResult> {
    let parts: Vec<&[u8]> = line.split(|&b| b == b'|').collect();
    if parts.len() < 6 {
        return None;
    }
    let text = |i: usize| -> String {
        let s = String::from_utf8_lossy(parts[i].trim_ascii());
        if s.chars().any(char::is_control) {
            s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
        } else {
            s.into_owned()
        }
    };
    let int = |i: usize| std::str::from_utf8(parts[i].trim_ascii()).ok().and_then(|s| s.parse::<i64>().ok());

    let site_url = text(5);
    Some(SearchResult {
        file_url: text(0),
        file_name: text(1),
        // Unparsable sizes become 0; -1 (unknown) passes through.
        file_size: int(2).unwrap_or(0),
        seeders: int(3).filter(|&v| v >= 0).unwrap_or(-1),
        leechers: int(4).filter(|&v| v >= 0).unwrap_or(-1),
        engine_name: engine_of(&site_url),
        descr_link: if parts.len() > 6 { text(6) } else { String::new() },
        pub_date: if parts.len() > 7 { int(7).filter(|&v| v > 0).unwrap_or(-1) } else { -1 },
        site_url,
    })
}

/// Splits a byte stream into `\n`-terminated lines, discarding lines longer
/// than `max` bytes. An unterminated tail is never emitted.
pub(crate) struct LineSplitter {
    buf: Vec<u8>,
    max: usize,
    overflow: bool,
}

impl LineSplitter {
    pub fn new(max: usize) -> Self {
        Self { buf: Vec::new(), max, overflow: false }
    }

    pub fn push(&mut self, mut data: &[u8], mut f: impl FnMut(&[u8])) {
        while !data.is_empty() {
            match data.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    if !self.overflow && self.buf.len() + i <= self.max {
                        self.buf.extend_from_slice(&data[..i]);
                        f(&self.buf);
                    }
                    self.buf.clear();
                    self.overflow = false;
                    data = &data[i + 1..];
                }
                None => {
                    if !self.overflow {
                        if self.buf.len() + data.len() > self.max {
                            self.buf.clear();
                            self.buf.shrink_to(self.max.min(4096));
                            self.overflow = true;
                        } else {
                            self.buf.extend_from_slice(data);
                        }
                    }
                    data = &[];
                }
            }
        }
    }
}

/// `search/results` paging: `limit <= 0` means all; a negative `offset`
/// counts from the end; an offset past the end is a conflict.
pub(crate) fn page_range(total: usize, limit: i64, offset: i64) -> QbtResult<Range<usize>> {
    let total_i = i64::try_from(total).unwrap_or(i64::MAX);
    let out_of_range = || QbtError::conflict("Offset is out of range");
    if offset > total_i {
        return Err(out_of_range());
    }
    let offset = if offset < 0 { total_i + offset } else { offset };
    if offset < 0 {
        return Err(out_of_range());
    }
    let start = usize::try_from(offset).map_err(|_| out_of_range())?;
    let end = if limit <= 0 { total } else { start.saturating_add(usize::try_from(limit).unwrap_or(usize::MAX)).min(total) };
    Ok(start..end)
}

fn result_size(r: &SearchResult) -> usize {
    std::mem::size_of::<SearchResult>()
        + r.file_name.len()
        + r.file_url.len()
        + r.engine_name.len()
        + r.site_url.len()
        + r.descr_link.len()
}

/// Truncates long error text (stderr excerpts) for logs and messages.
pub(crate) fn cap_text(s: &str) -> String {
    if s.len() <= ERROR_TEXT_CAP {
        return s.to_string();
    }
    let mut end = ERROR_TEXT_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[derive(Debug, Default)]
pub(crate) struct JobState {
    pub results: Vec<SearchResult>,
    pub bytes: usize,
    pub running: bool,
    pub outcome: SearchJobOutcome,
    pub error: String,
    /// Results discarded because of the memory caps.
    pub dropped: u64,
}

/// A search job and its results.
pub(crate) struct Job {
    pub id: i64,
    pub pattern: String,
    pub category: String,
    pub plugins: Vec<String>,
    pub state: Mutex<JobState>,
    stop_requested: AtomicBool,
    stop: Notify,
    pub task: Mutex<Option<JoinHandle<()>>>,
}

impl Job {
    pub fn new(id: i64, pattern: String, category: String, plugins: Vec<String>) -> Self {
        Self {
            id,
            pattern,
            category,
            plugins,
            state: Mutex::new(JobState { running: true, ..Default::default() }),
            stop_requested: AtomicBool::new(false),
            stop: Notify::new(),
            task: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().running
    }

    /// Asks the job to terminate its process (no-op when already finished).
    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        self.stop.notify_one();
    }

    pub fn info(&self) -> SearchJobInfo {
        let st = self.state.lock();
        SearchJobInfo {
            id: self.id,
            pattern: self.pattern.clone(),
            status: if st.running { SearchJobStatus::Running } else { SearchJobStatus::Stopped },
            total: st.results.len() as i64,
            outcome: st.outcome,
            error: st.error.clone(),
        }
    }

    pub fn page(&self, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
        let st = self.state.lock();
        let range = page_range(st.results.len(), limit, offset)?;
        Ok(SearchResultsPage {
            status: if st.running { SearchJobStatus::Running } else { SearchJobStatus::Stopped },
            total: st.results.len() as i64,
            results: st.results[range].to_vec(),
        })
    }

    /// Stores parsed results within the caps. Returns true if any was kept.
    fn store(&self, results: Vec<SearchResult>) -> bool {
        if results.is_empty() {
            return false;
        }
        let mut st = self.state.lock();
        let mut kept = false;
        for r in results {
            let size = result_size(&r);
            if st.results.len() >= MAX_RESULTS_PER_JOB || st.bytes + size > MAX_JOB_BYTES {
                st.dropped += 1;
                continue;
            }
            st.bytes += size;
            st.results.push(r);
            kept = true;
        }
        kept
    }

    fn finish(&self, outcome: SearchJobOutcome, error: String) {
        let mut st = self.state.lock();
        st.running = false;
        st.outcome = match outcome {
            SearchJobOutcome::Finished if st.results.is_empty() => SearchJobOutcome::NoResults,
            o => o,
        };
        st.error = error;
    }
}

/// Everything a job needs to run its process.
pub(crate) struct JobLaunch {
    pub python: PathBuf,
    pub nova_root: PathBuf,
    pub nova2: PathBuf,
    pub env: ProxyEnv,
    /// Plugin site URL -> plugin name (`pluginNameBySiteURL`).
    pub engines_by_url: HashMap<String, String>,
    pub events: EventBus,
    pub logger: Arc<Logger>,
    pub timeout: Duration,
}

/// Runs the job's `nova2.py` process to completion and records the outcome.
pub(crate) async fn run_job(job: Arc<Job>, l: JobLaunch) {
    let engines = job.plugins.join(",");
    let describe = |error: &str| {
        format!(
            "Search query: \"{}\". Category: \"{}\". Engines: \"{}\". Error: \"{}\".",
            job.pattern,
            job.category,
            job.plugins.join(", "),
            error
        )
    };
    let emit = || l.events.emit(CoreEvent::SearchUpdated { id: job.id });

    if job.stop_requested.load(Ordering::SeqCst) {
        job.finish(SearchJobOutcome::Aborted, String::new());
        emit();
        return;
    }

    let mut args: Vec<String> = vec![l.nova2.to_string_lossy().into_owned(), engines, job.category.clone()];
    // Like qBittorrent: one argv entry per space-separated word.
    args.extend(job.pattern.split(' ').map(str::to_string));
    let mut child = match python_command(&l.python, &l.nova_root, &l.env, &args).spawn() {
        Ok(c) => c,
        Err(e) => {
            let msg = "Process failed to start";
            l.logger.warning(format!("Search process failed. {}", describe(&format!("{msg}: {e}"))));
            job.finish(SearchJobOutcome::Error, msg.to_string());
            emit();
            return;
        }
    };
    // Kills the process group if this task is dropped before reaping it.
    let mut guard = GroupGuard::new(child.id());

    let mut stdout = child.stdout.take();
    let stderr_fut = read_capped(child.stderr.take(), STDERR_CAP);
    tokio::pin!(stderr_fut);
    let mut stderr_buf: Option<Vec<u8>> = None;

    let engine_of = |url: &str| l.engines_by_url.get(url).cloned().unwrap_or_default();
    let mut splitter = LineSplitter::new(MAX_LINE_LEN);
    let mut buf = vec![0u8; 16 * 1024];
    let mut ingest = |data: &[u8]| -> bool {
        let mut parsed = Vec::new();
        splitter.push(data, |line| {
            if let Some(r) = parse_result_line(line, engine_of) {
                parsed.push(r);
            }
        });
        job.store(parsed)
    };

    let deadline = Instant::now() + l.timeout;
    let mut pending_emit = false;
    let mut last_emit = Instant::now();
    let mut cancelled = false;

    let status: Option<ExitStatus> = loop {
        tokio::select! {
            biased;
            _ = job.stop.notified() => {
                cancelled = true;
                break terminate(&mut child).await;
            }
            _ = tokio::time::sleep_until(deadline) => {
                cancelled = true;
                break terminate(&mut child).await;
            }
            st = child.wait() => break st.ok(),
            n = read_some(&mut stdout, &mut buf) => {
                match n {
                    Some(n) => pending_emit |= ingest(&buf[..n]),
                    None => stdout = None,
                }
            }
            out = &mut stderr_fut, if stderr_buf.is_none() => stderr_buf = Some(out),
            _ = tokio::time::sleep_until(last_emit + EMIT_INTERVAL), if pending_emit => {
                pending_emit = false;
                last_emit = Instant::now();
                emit();
            }
        }
    };
    guard.disarm();

    if !cancelled {
        // The process exited: read what is left in the pipe (bounded, in case
        // a stray grandchild keeps it open).
        let drain = async {
            while let Some(n) = read_some(&mut stdout, &mut buf).await {
                ingest(&buf[..n]);
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(5), drain).await;
    }
    drop(stdout);
    let stderr_bytes = match stderr_buf {
        Some(b) => b,
        None => tokio::time::timeout(Duration::from_secs(2), &mut stderr_fut).await.unwrap_or_default(),
    };

    let err_text = cap_text(String::from_utf8_lossy(&stderr_bytes).trim());
    if !err_text.is_empty() {
        l.logger.warning(format!("Error occurred in search engine. {}", describe(&err_text)));
    }
    let dropped = job.state.lock().dropped;
    if dropped > 0 {
        l.logger.info(format!(
            "Search job {} reached the result limit; {dropped} results were discarded. Search query: \"{}\".",
            job.id, job.pattern
        ));
    }

    let (outcome, error) = if cancelled {
        (SearchJobOutcome::Aborted, String::new())
    } else {
        match status {
            Some(st) if st.success() => (SearchJobOutcome::Finished, String::new()),
            Some(st) if st.code().is_none() => {
                l.logger.warning(format!("Search process failed. {}", describe("Process crashed")));
                (SearchJobOutcome::Error, if err_text.is_empty() { "Process crashed".to_string() } else { err_text })
            }
            Some(st) => {
                let msg = if err_text.is_empty() { format!("Search engine exited with code {}", st.code().unwrap_or(-1)) } else { err_text };
                (SearchJobOutcome::Error, msg)
            }
            None => (SearchJobOutcome::Error, "Process unknown error".to_string()),
        }
    };
    job.finish(outcome, error);
    emit();
}

/// Reads the next chunk from an optional pipe; `None` at EOF/error (and
/// forever pending once the pipe is gone, so `select!` ignores it).
async fn read_some(pipe: &mut Option<tokio::process::ChildStdout>, buf: &mut [u8]) -> Option<usize> {
    match pipe {
        Some(p) => match p.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_of(url: &str) -> String {
        if url == "https://site.example" { "site".into() } else { String::new() }
    }

    #[test]
    fn parses_full_line() {
        let r = parse_result_line(
            b"magnet:?xt=urn:btih:abc|Some Name|1610612736|12|3|https://site.example|https://site.example/t/1|1700000000",
            engine_of,
        )
        .unwrap();
        assert_eq!(r.file_url, "magnet:?xt=urn:btih:abc");
        assert_eq!(r.file_name, "Some Name");
        assert_eq!(r.file_size, 1_610_612_736);
        assert_eq!(r.seeders, 12);
        assert_eq!(r.leechers, 3);
        assert_eq!(r.site_url, "https://site.example");
        assert_eq!(r.engine_name, "site");
        assert_eq!(r.descr_link, "https://site.example/t/1");
        assert_eq!(r.pub_date, 1_700_000_000);
    }

    #[test]
    fn field_rules() {
        // Exactly six fields: optional ones default.
        let r = parse_result_line(b" http://x/a.torrent | Name |-1| -5 |abc| https://other ", engine_of).unwrap();
        assert_eq!(r.file_url, "http://x/a.torrent");
        assert_eq!(r.file_name, "Name");
        assert_eq!(r.file_size, -1);
        assert_eq!(r.seeders, -1, "negative seeds become -1");
        assert_eq!(r.leechers, -1, "unparsable leechers become -1");
        assert_eq!(r.engine_name, "", "unknown site URL");
        assert_eq!(r.descr_link, "");
        assert_eq!(r.pub_date, -1);

        let r = parse_result_line(b"l|n|12 GB|+4|0|u|d|0|extra|fields", engine_of).unwrap();
        assert_eq!(r.file_size, 0, "unparsable size becomes 0");
        assert_eq!(r.seeders, 4);
        assert_eq!(r.leechers, 0);
        assert_eq!(r.descr_link, "d");
        assert_eq!(r.pub_date, -1, "non-positive dates are unknown");

        let r = parse_result_line(b"l|n|1|1|1|u|d|-1\r", engine_of).unwrap();
        assert_eq!(r.pub_date, -1);
        let r = parse_result_line(b"l|n|99999999999999999999|1|1|u||notanumber", engine_of).unwrap();
        assert_eq!(r.file_size, 0, "overflow becomes 0");
        assert_eq!(r.pub_date, -1);
    }

    #[test]
    fn bad_lines() {
        assert!(parse_result_line(b"", engine_of).is_none());
        assert!(parse_result_line(b"a|b|c|d|e", engine_of).is_none());
        assert!(parse_result_line(b"Traceback (most recent call last):", engine_of).is_none());
        // Invalid UTF-8 and control characters are sanitized, not fatal.
        let r = parse_result_line(b"l|na\xffme\x1b[31m|1|1|1|u", engine_of).unwrap();
        assert_eq!(r.file_name, "na\u{fffd}me [31m");
    }

    #[test]
    fn line_splitter() {
        let mut s = LineSplitter::new(10);
        let mut lines: Vec<Vec<u8>> = Vec::new();
        s.push(b"ab", |l| lines.push(l.to_vec()));
        s.push(b"c\nde\n\nf", |l| lines.push(l.to_vec()));
        assert_eq!(lines, vec![b"abc".to_vec(), b"de".to_vec(), b"".to_vec()]);
        lines.clear();
        // "f" + a long continuation is dropped as a whole, then parsing resumes.
        s.push(b"0123456789abc", |l| lines.push(l.to_vec()));
        s.push(b"def\nok\n", |l| lines.push(l.to_vec()));
        assert_eq!(lines, vec![b"ok".to_vec()]);
        lines.clear();
        s.push(b"0123456789X\nfine\n", |l| lines.push(l.to_vec()));
        assert_eq!(lines, vec![b"fine".to_vec()]);
        lines.clear();
        s.push(b"0123456789\n", |l| lines.push(l.to_vec()));
        assert_eq!(lines, vec![b"0123456789".to_vec()], "exactly max is kept");
    }

    #[test]
    fn paging() {
        assert_eq!(page_range(10, 0, 0).unwrap(), 0..10);
        assert_eq!(page_range(10, -3, 0).unwrap(), 0..10);
        assert_eq!(page_range(10, 3, 0).unwrap(), 0..3);
        assert_eq!(page_range(10, 3, 8).unwrap(), 8..10);
        assert_eq!(page_range(10, 0, 10).unwrap(), 10..10);
        assert_eq!(page_range(10, 0, -3).unwrap(), 7..10);
        assert_eq!(page_range(10, 2, -3).unwrap(), 7..9);
        assert_eq!(page_range(10, 0, -10).unwrap(), 0..10);
        assert_eq!(page_range(10, i64::MAX, 5).unwrap(), 5..10);
        assert!(page_range(10, 0, 11).is_err());
        assert!(page_range(10, 0, -11).is_err());
        assert!(page_range(0, 0, i64::MIN).is_err());
        assert_eq!(page_range(0, 0, 0).unwrap(), 0..0);
        assert!(matches!(page_range(1, 0, 2), Err(QbtError::Conflict { .. })));
    }

    #[test]
    fn job_caps_and_outcome() {
        let job = Job::new(1, "p".into(), "all".into(), vec![]);
        let r = parse_result_line(b"l|n|1|1|1|u", engine_of).unwrap();
        assert!(job.store(vec![r.clone(); 3]));
        assert_eq!(job.info().total, 3);
        {
            let mut st = job.state.lock();
            st.bytes = MAX_JOB_BYTES;
        }
        assert!(!job.store(vec![r]));
        assert_eq!(job.state.lock().dropped, 1);
        job.finish(SearchJobOutcome::Finished, String::new());
        let info = job.info();
        assert_eq!(info.status, SearchJobStatus::Stopped);
        assert_eq!(info.outcome, SearchJobOutcome::Finished);

        let empty = Job::new(2, "p".into(), "all".into(), vec![]);
        empty.finish(SearchJobOutcome::Finished, String::new());
        assert_eq!(empty.info().outcome, SearchJobOutcome::NoResults);
    }
}
