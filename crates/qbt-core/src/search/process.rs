//! Spawning Python child processes safely: argv only (never a shell), stdin
//! closed, own process group so the whole tree (nova2's multiprocessing
//! workers) can be terminated, bounded output and timeouts.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use super::proxy::ProxyEnv;

/// `PYTHON_ISOLATE_MODE_FLAG`, `PYTHON_UTF8_MODE_FLAG`.
pub(crate) const PYTHON_FLAGS: [&str; 2] = ["-I", "-Xutf8=1"];

/// Grace period between SIGTERM and SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);

/// Builds `<python> -I -Xutf8=1 <args...>` running in `cwd` with the proxy
/// environment. Each argument is a separate argv entry.
pub(crate) fn python_command<I, S>(python: &Path, cwd: &Path, env: &ProxyEnv, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(python);
    cmd.args(PYTHON_FLAGS).args(args).current_dir(cwd);
    env.apply(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.process_group(0);
    cmd
}

/// Sends `sig` to the process group led by `pid` (the child was started
/// with `process_group(0)`, so its pid is the group id).
pub(crate) fn signal_group(pid: Option<u32>, sig: libc::c_int) {
    if let Some(pid) = pid.and_then(|p| libc::pid_t::try_from(p).ok()).filter(|&p| p > 1) {
        // SAFETY: plain syscall; `pid` is the group of a child we spawned and
        // have not reaped yet (callers guarantee), so it cannot be recycled.
        unsafe {
            libc::killpg(pid, sig);
        }
    }
}

/// Terminates a child and its whole process group (SIGTERM, then SIGKILL
/// after a grace period) and reaps it.
///
/// The group is signalled before the child is reaped: until then its pid
/// (= group id) cannot be recycled, even if the child already exited and
/// only grandchildren hold its pipes. `Child::id()` is `None` once tokio has
/// reaped the child, in which case no signal is sent.
pub(crate) async fn terminate(child: &mut Child) -> Option<ExitStatus> {
    let pid = child.id();
    signal_group(pid, libc::SIGTERM);
    match tokio::time::timeout(TERM_GRACE, child.wait()).await {
        Ok(st) => st.ok(),
        Err(_) => {
            signal_group(pid, libc::SIGKILL);
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    }
}

/// Kills the process group of a spawned child when dropped while armed, so
/// an aborted task (shutdown, cancelled future) never leaves nova2's worker
/// processes behind. Disarm it as soon as the child has been reaped.
pub(crate) struct GroupGuard {
    pid: Option<u32>,
}

impl GroupGuard {
    pub fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    pub fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        // The guard is declared after the `Child`, so it drops first: the
        // child is not reaped yet and its group id cannot be recycled.
        signal_group(self.pid.take(), libc::SIGKILL);
    }
}

/// Reads a stream to the end, keeping at most `cap` bytes (the rest is
/// drained and discarded so the child never blocks on a full pipe).
pub(crate) async fn read_capped<R: AsyncRead + Unpin>(r: Option<R>, cap: usize) -> Vec<u8> {
    let Some(mut r) = r else { return Vec::new() };
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = cap.saturating_sub(out.len());
                out.extend_from_slice(&buf[..n.min(room)]);
            }
        }
    }
    out
}

/// Output of a finished helper process.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// `None` when the process was killed because of the timeout.
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl Output {
    pub fn success(&self) -> bool {
        !self.timed_out && self.status.is_some_and(|s| s.success())
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Runs a prepared command to completion with a timeout and bounded output.
pub(crate) async fn run(mut cmd: Command, timeout: Duration, stdout_cap: usize, stderr_cap: usize) -> std::io::Result<Output> {
    let mut child = cmd.spawn()?;
    let mut guard = GroupGuard::new(child.id());
    let out_task = read_capped(child.stdout.take(), stdout_cap);
    let err_task = read_capped(child.stderr.take(), stderr_cap);
    let collect = async { tokio::join!(out_task, err_task) };
    tokio::pin!(collect);

    // Wait for both pipes to close (the process and anything it spawned
    // exited or closed them), bounded by the timeout.
    let output = match tokio::time::timeout(timeout, &mut collect).await {
        Ok((stdout, stderr)) => match tokio::time::timeout(timeout, child.wait()).await {
            Ok(st) => Output { status: st.ok(), stdout, stderr, timed_out: false },
            Err(_) => {
                terminate(&mut child).await;
                Output { status: None, stdout, stderr, timed_out: true }
            }
        },
        Err(_) => {
            terminate(&mut child).await;
            // Pipes are closed now that the group is gone; collect what was read.
            let (stdout, stderr) = tokio::time::timeout(Duration::from_secs(2), &mut collect).await.unwrap_or_default();
            Output { status: None, stdout, stderr, timed_out: true }
        }
    };
    guard.disarm();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capped_reader_drains() {
        let data = vec![b'x'; 100_000];
        let out = read_capped(Some(&data[..]), 10).await;
        assert_eq!(out.len(), 10);
    }

    #[tokio::test]
    async fn run_times_out_and_kills_group() {
        let mut cmd = Command::new("/bin/sh");
        // The shell spawns a background grandchild that keeps stdout open.
        cmd.args(["-c", "sleep 30 & echo started; sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let started = std::time::Instant::now();
        let out = run(cmd, Duration::from_millis(500), 1024, 1024).await.unwrap();
        assert!(out.timed_out);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "started");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn run_collects_output() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo err 1>&2; exit 3"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let out = run(cmd, Duration::from_secs(10), 1024, 1024).await.unwrap();
        assert!(!out.timed_out);
        assert!(!out.success());
        assert_eq!(out.status.and_then(|s| s.code()), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
        assert_eq!(out.stderr_text(), "err");
    }
}
