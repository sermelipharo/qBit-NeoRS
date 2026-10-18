//! Locating a Python 3 interpreter (`Utils::ForeignApps::pythonInfo()`),
//! macOS flavour: the "Python executable path" preference, else `python3` /
//! `python` in `PATH`, else the usual install locations.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::logger::Logger;
use crate::model::LogLevel;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Common install locations tried after `PATH` (GUI apps get a minimal PATH).
const COMMON_LOCATIONS: &[&str] = &[
    "/opt/homebrew/bin/python3",
    "/usr/local/bin/python3",
    "/opt/local/bin/python3",
    "/Library/Frameworks/Python.framework/Versions/Current/bin/python3",
    "/usr/bin/python3",
];

/// `PythonInfo::Version` (`Utils::Version<3, 1>`): 1 to 3 numeric components.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PythonVersion(pub [i32; 3]);

impl PythonVersion {
    /// `PythonInfo::MINIMUM_SUPPORTED_VERSION`.
    pub const MINIMUM: PythonVersion = PythonVersion([3, 9, 0]);

    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let mut v = [0i32; 3];
        for (i, p) in parts.iter().enumerate() {
            v[i] = p.trim().parse().ok()?;
        }
        let version = PythonVersion(v);
        version.is_valid().then_some(version)
    }

    pub fn is_valid(&self) -> bool {
        self.0.iter().all(|&c| c >= 0) && self.0.iter().any(|&c| c > 0)
    }

    pub fn is_supported(&self) -> bool {
        *self >= Self::MINIMUM
    }
}

impl fmt::Display for PythonVersion {
    /// Trailing zero components are omitted ("3.9", "3.12.1").
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut last = 2;
        while last > 0 && self.0[last] == 0 {
            last -= 1;
        }
        write!(f, "{}", self.0[0])?;
        for c in &self.0[1..=last] {
            write!(f, ".{c}")?;
        }
        Ok(())
    }
}

/// Parses `python --version` output ("Python 3.12.1", "Python 3.6.6+",
/// "Python 3.4.3 :: Anaconda 2.3.0 (64-bit)").
pub(crate) fn parse_version_output(output: &[u8]) -> Option<PythonVersion> {
    let text = String::from_utf8_lossy(output);
    let mut tokens = text.split_whitespace();
    tokens.next()?;
    let token = tokens.next()?;
    let end = token.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(token.len());
    PythonVersion::parse(&token[..end])
}

/// A found interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PythonInfo {
    pub path: PathBuf,
    pub version: PythonVersion,
}

impl PythonInfo {
    pub fn is_supported(&self) -> bool {
        self.version.is_supported()
    }
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// Resolves a bare executable name through `PATH`.
pub(crate) fn find_in_path(name: &str, path_var: Option<&OsString>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(name))
        .find(|p| is_executable_file(p))
}

/// On macOS `/usr/bin/python3` is a stub that opens the "install command
/// line developer tools" dialog when the tools are missing; only use it
/// when a real interpreter is behind it.
async fn is_macos_stub(path: &Path) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if canonical != Path::new("/usr/bin/python3") {
        return false;
    }
    if Path::new("/Library/Developer/CommandLineTools/usr/bin/python3").exists() {
        return false;
    }
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match out {
        Ok(Ok(o)) if o.status.success() => {
            let dir = String::from_utf8_lossy(&o.stdout).trim().to_string();
            dir.is_empty() || !Path::new(&dir).join("usr/bin/python3").exists()
        }
        _ => true,
    }
}

/// `testPythonInstallation()`: runs `<exe> --version` and parses the version.
pub(crate) async fn test_installation(path: &Path) -> Option<PythonInfo> {
    if !is_executable_file(path) || is_macos_stub(path).await {
        return None;
    }
    let mut cmd = tokio::process::Command::new(path);
    cmd.arg("--version").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let out = tokio::time::timeout(VERSION_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    // Python 2 printed the version on stderr.
    let text = if out.stdout.iter().any(|b| !b.is_ascii_whitespace()) { &out.stdout } else { &out.stderr };
    let version = parse_version_output(text)?;
    Some(PythonInfo { path: path.to_path_buf(), version })
}

fn expand_tilde(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

/// `Utils::ForeignApps::pythonInfo()`. `preferred` is the
/// `python_executable_path` preference: when set, only it is tested.
/// Returns the interpreter found (possibly too old), or `None`.
/// `quiet` logs the final "not found" at info level (start-up probe).
pub(crate) async fn detect(preferred: &str, logger: &Logger, quiet: bool) -> Option<PythonInfo> {
    let too_old = |info: &PythonInfo, level: LogLevel| {
        logger.log(
            level,
            format!(
                "Python failed to meet minimum version requirement. Path: \"{}\". Found version: \"{}\". Minimum supported version: \"{}\".",
                info.path.display(),
                info.version,
                PythonVersion::MINIMUM
            ),
        );
    };
    let found = |info: &PythonInfo| {
        logger.info(format!("Found Python executable. Name: \"{}\". Version: \"{}\"", info.path.display(), info.version));
    };

    let path_var = std::env::var_os("PATH");
    let preferred = preferred.trim();
    if !preferred.is_empty() {
        let candidate = if preferred.contains('/') {
            Some(expand_tilde(preferred))
        } else {
            find_in_path(preferred, path_var.as_ref())
        };
        match candidate {
            Some(path) => match test_installation(&path).await {
                Some(info) => {
                    found(&info);
                    if !info.is_supported() {
                        too_old(&info, LogLevel::Warning);
                    }
                    return Some(info);
                }
                None => logger.warning(format!("Failed to find Python executable. Path: \"{preferred}\".")),
            },
            None => logger.warning(format!("Failed to find Python executable. Path: \"{preferred}\".")),
        }
        return None;
    }

    let mut best_old: Option<PythonInfo> = None;
    let mut tried: Vec<PathBuf> = Vec::new();
    for name in ["python3", "python"] {
        match find_in_path(name, path_var.as_ref()) {
            Some(path) => {
                tried.push(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()));
                match test_installation(&path).await {
                    Some(info) if info.is_supported() => {
                        found(&info);
                        return Some(info);
                    }
                    Some(info) => {
                        found(&info);
                        too_old(&info, LogLevel::Info);
                        best_old.get_or_insert(info);
                    }
                    None => logger.info(format!(
                        "Failed to find `{name}` executable in PATH environment variable. PATH: \"{}\"",
                        path_var.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
                    )),
                }
            }
            None => logger.info(format!(
                "Failed to find `{name}` executable in PATH environment variable. PATH: \"{}\"",
                path_var.as_deref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
            )),
        }
    }

    for loc in COMMON_LOCATIONS {
        let path = PathBuf::from(loc);
        let canonical = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if tried.contains(&canonical) {
            continue;
        }
        tried.push(canonical);
        match test_installation(&path).await {
            Some(info) if info.is_supported() => {
                found(&info);
                return Some(info);
            }
            Some(info) => {
                found(&info);
                too_old(&info, LogLevel::Info);
                best_old.get_or_insert(info);
            }
            None => {}
        }
    }

    if best_old.is_none() {
        logger.log(if quiet { LogLevel::Info } else { LogLevel::Warning }, "Failed to find Python executable");
    }
    best_old
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing() {
        assert_eq!(PythonVersion::parse("3.12.1"), Some(PythonVersion([3, 12, 1])));
        assert_eq!(PythonVersion::parse("3.9"), Some(PythonVersion([3, 9, 0])));
        assert_eq!(PythonVersion::parse("3"), Some(PythonVersion([3, 0, 0])));
        assert_eq!(PythonVersion::parse("3.1.2.3"), None);
        assert_eq!(PythonVersion::parse(""), None);
        assert_eq!(PythonVersion::parse("0.0"), None);
        assert_eq!(PythonVersion::parse("3."), None);
        assert_eq!(PythonVersion([3, 9, 0]).to_string(), "3.9");
        assert_eq!(PythonVersion([3, 0, 0]).to_string(), "3");
        assert_eq!(PythonVersion([3, 12, 1]).to_string(), "3.12.1");
        assert_eq!(PythonVersion([3, 0, 1]).to_string(), "3.0.1");
    }

    #[test]
    fn version_support() {
        assert!(PythonVersion([3, 9, 0]).is_supported());
        assert!(PythonVersion([3, 14, 2]).is_supported());
        assert!(PythonVersion([4, 0, 0]).is_supported());
        assert!(!PythonVersion([3, 8, 18]).is_supported());
        assert!(!PythonVersion([2, 7, 18]).is_supported());
    }

    #[test]
    fn version_output() {
        assert_eq!(parse_version_output(b"Python 3.12.1\n"), Some(PythonVersion([3, 12, 1])));
        assert_eq!(parse_version_output(b"Python 3.6.6+"), Some(PythonVersion([3, 6, 6])));
        assert_eq!(parse_version_output(b"Python 3.4.3 :: Anaconda 2.3.0 (64-bit)"), Some(PythonVersion([3, 4, 3])));
        assert_eq!(parse_version_output(b"  Python   3.13.0rc1 \r\n"), Some(PythonVersion([3, 13, 0])));
        assert_eq!(parse_version_output(b"Python"), None);
        assert_eq!(parse_version_output(b""), None);
        assert_eq!(parse_version_output(b"zsh: command not found"), None);
    }

    #[test]
    fn path_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("python3");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        let path_var = std::env::join_paths(["relative/dir".into(), tmp.path().to_path_buf()]).unwrap();
        assert_eq!(find_in_path("python3", Some(&path_var)), None, "not executable");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_in_path("python3", Some(&path_var)), Some(exe));
        assert_eq!(find_in_path("python", Some(&path_var)), None);
        assert_eq!(find_in_path("python3", None), None);
    }
}
