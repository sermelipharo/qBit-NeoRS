//! The bundled nova3 helper scripts, their installation into `data/nova3`
//! (`SearchPluginManager::updateNova()`), plugin version headers and Python
//! cache cleanup.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::QbtResult;
use crate::utils::fs::write_atomic;

/// Helper scripts shipped with the application (qBittorrent's
/// `searchengine.qrc`). Each carries a `# VERSION: x.y` header; a bundled
/// script replaces the copy on disk only when its version is newer.
pub(crate) const BUNDLED_FILES: &[(&str, &str)] = &[
    ("helpers.py", include_str!("../../resources/nova3/helpers.py")),
    ("nova2.py", include_str!("../../resources/nova3/nova2.py")),
    ("nova2dl.py", include_str!("../../resources/nova3/nova2dl.py")),
    ("novaprinter.py", include_str!("../../resources/nova3/novaprinter.py")),
    ("socks.py", include_str!("../../resources/nova3/socks.py")),
];

/// Plugin files larger than this are not read (official plugins are a few KiB).
pub(crate) const MAX_PLUGIN_FILE_SIZE: u64 = 2 * 1024 * 1024;

/// `PluginVersion` (`Utils::Version<2>`): exactly `major.minor`, numeric.
/// The default value (0.0) is the "invalid" version.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct PluginVersion {
    pub major: i32,
    pub minor: i32,
}

impl PluginVersion {
    #[cfg(test)]
    pub fn new(major: i32, minor: i32) -> Self {
        Self { major, minor }
    }

    /// `Version::fromString`: two dot-separated integers (surrounding
    /// whitespace allowed per component). Returns `None` when malformed or
    /// not valid (negative component, or 0.0).
    pub fn parse(s: &str) -> Option<Self> {
        let mut parts = s.split('.');
        let major = parts.next()?.trim().parse::<i32>().ok()?;
        let minor = parts.next()?.trim().parse::<i32>().ok()?;
        if parts.next().is_some() {
            return None;
        }
        let v = Self { major, minor };
        v.is_valid().then_some(v)
    }

    pub fn is_valid(&self) -> bool {
        self.major >= 0 && self.minor >= 0 && (self.major > 0 || self.minor > 0)
    }
}

impl PartialOrd for PluginVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PluginVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor).cmp(&(other.major, other.minor))
    }
}

impl fmt::Display for PluginVersion {
    /// `Version<2>::toString()` always prints both components ("1.0", "2.5").
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Result of scanning a plugin for its `# VERSION:` header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum VersionHeader {
    Valid(PluginVersion),
    /// A header exists but its value is malformed (the raw value is kept for
    /// the "contains invalid version string" warning).
    Invalid(String),
    Missing,
}

impl VersionHeader {
    pub fn version(&self) -> PluginVersion {
        match self {
            VersionHeader::Valid(v) => *v,
            _ => PluginVersion::default(),
        }
    }
}

/// `SearchPluginManager::getPluginVersion()`: the file is read like Qt's
/// `readLine(16)` (at most 15 bytes per read, stopping after `\n`), spaces
/// are removed, and the first chunk starting with `#VERSION:`
/// (case-insensitive) holds the version.
pub(crate) fn parse_version_header(content: &[u8]) -> VersionHeader {
    const CHUNK: usize = 15;
    let mut pos = 0;
    while pos < content.len() {
        let window = &content[pos..content.len().min(pos + CHUNK)];
        let len = window.iter().position(|&b| b == b'\n').map_or(window.len(), |i| i + 1);
        let chunk = &content[pos..pos + len];
        pos += len;

        let line: String = String::from_utf8_lossy(chunk).chars().filter(|&c| c != ' ').collect();
        let bytes = line.as_bytes();
        if bytes.len() < 9 || !bytes[..9].eq_ignore_ascii_case(b"#VERSION:") {
            continue;
        }
        let value = &line[9..];
        return match PluginVersion::parse(value) {
            Some(v) => VersionHeader::Valid(v),
            None => VersionHeader::Invalid(value.trim().to_string()),
        };
    }
    VersionHeader::Missing
}

/// Reads the version header of a file on disk (missing/unreadable/too large
/// files have no version).
pub(crate) fn file_version_header(path: &Path) -> VersionHeader {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_PLUGIN_FILE_SIZE => {}
        _ => return VersionHeader::Missing,
    }
    match std::fs::read(path) {
        Ok(data) => parse_version_header(&data),
        Err(_) => VersionHeader::Missing,
    }
}

/// Folder layout of the search engine.
#[derive(Clone, Debug)]
pub(crate) struct NovaDirs {
    /// `data/nova3`
    pub root: PathBuf,
    /// `data/nova3/engines`
    pub engines: PathBuf,
}

impl NovaDirs {
    pub fn new(root: PathBuf) -> Self {
        let engines = root.join("engines");
        Self { root, engines }
    }

    pub fn nova2(&self) -> PathBuf {
        self.root.join("nova2.py")
    }

    pub fn nova2dl(&self) -> PathBuf {
        self.root.join("nova2dl.py")
    }

    /// `engines/<name>.py` (callers validate `name`).
    pub fn plugin_file(&self, name: &str) -> PathBuf {
        self.engines.join(format!("{name}.py"))
    }

    /// `engines/<name>.png`, else `engines/<name>.ico`, if present.
    pub fn icon_file(&self, name: &str) -> Option<PathBuf> {
        ["png", "ico"]
            .iter()
            .map(|ext| self.engines.join(format!("{name}.{ext}")))
            .find(|p| p.symlink_metadata().map(|m| m.is_file()).unwrap_or(false))
    }
}

/// `SearchPluginManager::updateNova()`: creates the package layout and
/// installs bundled helper scripts that are newer than the copies on disk.
pub(crate) fn update_nova(dirs: &NovaDirs) -> QbtResult<()> {
    std::fs::create_dir_all(&dirs.engines)?;
    // Empty package markers, truncated on every start like qBittorrent does.
    std::fs::write(dirs.root.join("__init__.py"), b"")?;
    std::fs::write(dirs.engines.join("__init__.py"), b"")?;

    for (name, content) in BUNDLED_FILES {
        let disk = dirs.root.join(name);
        let bundled_version = parse_version_header(content.as_bytes()).version();
        let disk_version = file_version_header(&disk).version();
        if bundled_version <= disk_version {
            continue;
        }
        write_atomic(&disk, content.as_bytes())?;
    }
    Ok(())
}

/// `clearPythonCache()`: removes every `__pycache__` folder and `*.pyc` file
/// below `root` (symlinks are never followed).
pub(crate) fn clear_python_cache(root: &Path) {
    fn walk(dir: &Path, depth: usize) {
        if depth > 8 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                if entry.file_name() == "__pycache__" {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    walk(&path, depth + 1);
                }
            } else if ft.is_file() && path.extension().is_some_and(|e| e == "pyc") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    walk(root, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_version_parsing() {
        assert_eq!(PluginVersion::parse("1.53"), Some(PluginVersion::new(1, 53)));
        assert_eq!(PluginVersion::parse(" 2.05\n"), Some(PluginVersion::new(2, 5)));
        assert_eq!(PluginVersion::parse("0.1"), Some(PluginVersion::new(0, 1)));
        assert_eq!(PluginVersion::parse("0.0"), None);
        assert_eq!(PluginVersion::parse("1"), None);
        assert_eq!(PluginVersion::parse("1.2.3"), None);
        assert_eq!(PluginVersion::parse("1.x"), None);
        assert_eq!(PluginVersion::parse("-1.2"), None);
        assert_eq!(PluginVersion::parse(""), None);
        assert_eq!(PluginVersion::new(2, 5).to_string(), "2.5");
        assert_eq!(PluginVersion::new(1, 0).to_string(), "1.0");
    }

    #[test]
    fn plugin_version_ordering() {
        assert!(PluginVersion::new(1, 10) > PluginVersion::new(1, 9));
        assert!(PluginVersion::new(2, 0) > PluginVersion::new(1, 99));
        assert!(PluginVersion::default() < PluginVersion::new(0, 1));
        assert_eq!(PluginVersion::parse("1.05"), PluginVersion::parse("1.5"));
    }

    #[test]
    fn version_header_parsing() {
        let v = |s: &str| parse_version_header(s.as_bytes());
        assert_eq!(v("# VERSION: 1.53\n\nimport x\n"), VersionHeader::Valid(PluginVersion::new(1, 53)));
        assert_eq!(v("#!/usr/bin/env python\n#VERSION: 2.10\n"), VersionHeader::Valid(PluginVersion::new(2, 10)));
        assert_eq!(v("# version: 1.2\r\n"), VersionHeader::Valid(PluginVersion::new(1, 2)));
        assert_eq!(v("import os\n"), VersionHeader::Missing);
        assert_eq!(v(""), VersionHeader::Missing);
        assert_eq!(v("# VERSION: abc\n"), VersionHeader::Invalid("abc".into()));
        // Qt reads at most 15 bytes per line: trailing text is ignored.
        assert_eq!(v("# VERSION: 1.53 # comment\n"), VersionHeader::Valid(PluginVersion::new(1, 53)));
        // A header that is not at the start of a line chunk does not count.
        assert_eq!(v("x = 1 # VERSION: 1.0\n"), VersionHeader::Missing);
    }

    #[test]
    fn bundled_files_have_versions() {
        for (name, content) in BUNDLED_FILES {
            assert!(
                matches!(parse_version_header(content.as_bytes()), VersionHeader::Valid(_)),
                "{name} has no version header"
            );
        }
    }

    #[test]
    fn update_nova_installs_and_upgrades() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = NovaDirs::new(tmp.path().join("nova3"));
        update_nova(&dirs).unwrap();
        for (name, content) in BUNDLED_FILES {
            assert_eq!(std::fs::read_to_string(dirs.root.join(name)).unwrap(), *content);
        }
        assert!(dirs.engines.join("__init__.py").is_file());

        // An older copy is replaced, a newer one is kept.
        std::fs::write(dirs.nova2(), "# VERSION: 1.00\nold\n").unwrap();
        std::fs::write(dirs.root.join("helpers.py"), "# VERSION: 99.0\ncustom\n").unwrap();
        update_nova(&dirs).unwrap();
        assert!(std::fs::read_to_string(dirs.nova2()).unwrap().contains("def main()"));
        assert!(std::fs::read_to_string(dirs.root.join("helpers.py")).unwrap().contains("custom"));
    }

    #[test]
    fn python_cache_is_cleared() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("__pycache__")).unwrap();
        std::fs::write(root.join("__pycache__/x.cpython-312.pyc"), b"x").unwrap();
        std::fs::create_dir_all(root.join("engines/__pycache__")).unwrap();
        std::fs::write(root.join("engines/old.pyc"), b"x").unwrap();
        std::fs::write(root.join("engines/keep.py"), b"x").unwrap();
        clear_python_cache(root);
        assert!(!root.join("__pycache__").exists());
        assert!(!root.join("engines/__pycache__").exists());
        assert!(!root.join("engines/old.pyc").exists());
        assert!(root.join("engines/keep.py").exists());
    }
}
