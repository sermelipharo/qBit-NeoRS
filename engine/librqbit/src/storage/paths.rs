//! Relative path handling for torrent files: validation of user supplied paths,
//! sanitization of torrent supplied names, and the on-disk "decorations"
//! (incomplete-file suffix, `.unwanted` folder).
//!
//! Every path the storage joins onto the output folder goes through one of the two
//! entry points here, so that neither a malicious torrent nor a rename request can
//! address anything outside the output folder:
//!
//! * [`validate_relative_path`] is strict and used for paths coming from the API
//!   (renames, persisted renames). It rejects anything suspicious.
//! * [`sanitize_component`] is lenient and used for names coming from torrent
//!   metadata. It rewrites suspicious components instead of rejecting the torrent.

use std::{
    borrow::Cow,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

/// Name of the folder files with priority 0 are kept in when the unwanted folder
/// option is on.
pub const UNWANTED_FOLDER_NAME: &str = ".unwanted";

/// The longest file name component most file systems accept (in bytes).
pub const MAX_COMPONENT_LEN: usize = 255;

/// Why a relative path was rejected.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    #[error("the path is empty")]
    Empty,
    #[error("absolute paths are not allowed: {0:?}")]
    Absolute(String),
    #[error("the path contains an empty component: {0:?}")]
    EmptyComponent(String),
    #[error("the path contains a \".\" or \"..\" component: {0:?}")]
    DotComponent(String),
    #[error("the path contains a NUL character: {0:?}")]
    Nul(String),
    #[error("the path contains an invalid character: {0:?}")]
    InvalidChar(String),
    #[error("a path component is longer than {MAX_COMPONENT_LEN} bytes: {0:?}")]
    TooLong(String),
}

fn is_separator(b: u8) -> bool {
    b == b'/' || (cfg!(windows) && b == b'\\')
}

#[cfg(windows)]
fn is_forbidden_char(c: char) -> bool {
    matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32
}

#[cfg(not(windows))]
fn is_forbidden_char(_c: char) -> bool {
    false
}

/// Validate a relative path supplied by the user (e.g. a rename target) and return
/// it in normalized form.
///
/// Rejected: empty paths, absolute paths (including Windows prefixes), empty
/// components (`a//b`, trailing separators), `.` and `..` components, NUL bytes,
/// and components longer than [`MAX_COMPONENT_LEN`] bytes. On Windows also the
/// characters that are invalid in file names there.
pub fn validate_relative_path(path: &Path) -> Result<PathBuf, PathError> {
    let display = || path.to_string_lossy().into_owned();
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        return Err(PathError::Empty);
    }
    if is_separator(bytes[0]) || path.has_root() || path.is_absolute() {
        return Err(PathError::Absolute(display()));
    }
    for component in bytes.split(|b| is_separator(*b)) {
        match component {
            b"" => return Err(PathError::EmptyComponent(display())),
            b"." | b".." => return Err(PathError::DotComponent(display())),
            c if c.contains(&0) => return Err(PathError::Nul(display())),
            c if c.len() > MAX_COMPONENT_LEN => return Err(PathError::TooLong(display())),
            _ => {}
        }
    }
    if path.to_string_lossy().chars().any(is_forbidden_char) {
        return Err(PathError::InvalidChar(display()));
    }
    // Belt and braces: whatever the platform parses out of it must be plain names.
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(c) => out.push(c),
            Component::Prefix(_) | Component::RootDir => {
                return Err(PathError::Absolute(display()));
            }
            Component::CurDir | Component::ParentDir => {
                return Err(PathError::DotComponent(display()));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(PathError::Empty);
    }
    Ok(out)
}

/// Make a single file name component coming from torrent metadata safe to use.
///
/// Separators, NUL and control characters (plus characters invalid on Windows when
/// built for it) are replaced by `_`; empty, `.` and `..` components become `_`;
/// over-long components are truncated (keeping the extension when possible).
pub fn sanitize_component(raw: &str) -> Cow<'_, str> {
    let needs_char_fix = raw
        .chars()
        .any(|c| c == '/' || c == '\\' || c == '\0' || (c as u32) < 32 || is_forbidden_char(c));
    let mut s: Cow<'_, str> = if needs_char_fix {
        Cow::Owned(
            raw.chars()
                .map(|c| {
                    if c == '/' || c == '\\' || c == '\0' || (c as u32) < 32 || is_forbidden_char(c)
                    {
                        '_'
                    } else {
                        c
                    }
                })
                .collect(),
        )
    } else {
        Cow::Borrowed(raw)
    };
    if s.is_empty() || s == "." || s == ".." {
        s = Cow::Borrowed("_");
    }
    if s.len() > MAX_COMPONENT_LEN {
        s = Cow::Owned(truncate_component(&s));
    }
    s
}

fn truncate_component(s: &str) -> String {
    fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
        while !s.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }
    // Keep a short extension if there is one.
    let ext = s
        .rfind('.')
        .map(|i| &s[i..])
        .filter(|e| e.len() > 1 && e.len() <= 16);
    match ext {
        Some(ext) => {
            let stem_len = floor_char_boundary(s, MAX_COMPONENT_LEN - ext.len());
            format!("{}{}", &s[..stem_len], ext)
        }
        None => s[..floor_char_boundary(s, MAX_COMPONENT_LEN)].to_owned(),
    }
}

/// Build a safe relative path out of torrent supplied components.
pub fn sanitized_torrent_path<'a>(components: impl IntoIterator<Item = Cow<'a, str>>) -> PathBuf {
    let mut out = PathBuf::new();
    for c in components {
        out.push(&*sanitize_component(&c));
    }
    if out.as_os_str().is_empty() {
        out.push("_");
    }
    out
}

/// Whether the parent folder of `path` is the unwanted folder.
pub fn is_in_unwanted_folder(path: &Path) -> bool {
    path.parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == OsStr::new(UNWANTED_FOLDER_NAME))
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn into_unwanted_folder(path: &Path) -> PathBuf {
    let file_name = path.file_name().unwrap_or(path.as_os_str());
    match path.parent() {
        Some(parent) => parent.join(UNWANTED_FOLDER_NAME).join(file_name),
        None => Path::new(UNWANTED_FOLDER_NAME).join(file_name),
    }
}

/// The relative on-disk path of a file given its user-visible relative path.
///
/// * `suffix` is appended when set (the caller decides whether it applies, i.e.
///   the file is incomplete and not empty);
/// * the file is placed into `<parent>/.unwanted/` when `unwanted` is set.
///
/// The suffix is applied first, then the folder, like qBittorrent does.
pub fn decorated_path(user_path: &Path, suffix: Option<&str>, unwanted: bool) -> PathBuf {
    let with_suffix = match suffix {
        Some(s) if !s.is_empty() => Cow::Owned(append_suffix(user_path, s)),
        _ => Cow::Borrowed(user_path),
    };
    if unwanted {
        into_unwanted_folder(&with_suffix)
    } else {
        with_suffix.into_owned()
    }
}

/// All on-disk variants a file may exist under, the plain name first.
///
/// Used to find existing data (e.g. after a restart with partially suffixed files,
/// or after the options changed while the program wasn't running).
pub fn path_variants(user_path: &Path, suffix: Option<&str>) -> Vec<PathBuf> {
    let suffix = suffix.filter(|s| !s.is_empty());
    let mut v = Vec::with_capacity(4);
    v.push(user_path.to_owned());
    if let Some(s) = suffix {
        v.push(decorated_path(user_path, Some(s), false));
    }
    v.push(decorated_path(user_path, None, true));
    if let Some(s) = suffix {
        v.push(decorated_path(user_path, Some(s), true));
    }
    v
}

/// Validate an incomplete-file suffix: it has to be a non-empty piece of a file name.
pub fn validate_suffix(suffix: &str) -> Result<(), PathError> {
    if suffix.is_empty() {
        return Err(PathError::Empty);
    }
    if suffix.bytes().any(|b| b == b'/' || b == b'\\') {
        return Err(PathError::InvalidChar(suffix.to_owned()));
    }
    if suffix.contains('\0') {
        return Err(PathError::Nul(suffix.to_owned()));
    }
    if suffix.len() > 64 {
        return Err(PathError::TooLong(suffix.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn ok(s: &str) -> PathBuf {
        validate_relative_path(Path::new(s)).unwrap()
    }

    fn err(s: &str) -> PathError {
        validate_relative_path(Path::new(s)).unwrap_err()
    }

    #[test]
    fn test_validate_accepts_normal_paths() {
        assert_eq!(ok("a"), PathBuf::from("a"));
        assert_eq!(ok("a/b/c.txt"), PathBuf::from("a/b/c.txt"));
        assert_eq!(
            ok("dir with spaces/файл.mkv"),
            PathBuf::from("dir with spaces/файл.mkv")
        );
        assert_eq!(ok(".hidden"), PathBuf::from(".hidden"));
        assert_eq!(ok("..."), PathBuf::from("..."));
        assert_eq!(ok("a/..b"), PathBuf::from("a/..b"));
    }

    #[test]
    fn test_validate_rejects_traversal_and_absolute() {
        assert_eq!(err(""), PathError::Empty);
        assert!(matches!(err("/etc/passwd"), PathError::Absolute(_)));
        assert!(matches!(err("../x"), PathError::DotComponent(_)));
        assert!(matches!(err("a/../../x"), PathError::DotComponent(_)));
        assert!(matches!(err("a/.."), PathError::DotComponent(_)));
        assert!(matches!(err("./a"), PathError::DotComponent(_)));
        assert!(matches!(err("a/./b"), PathError::DotComponent(_)));
        assert!(matches!(err("."), PathError::DotComponent(_)));
        assert!(matches!(err("a//b"), PathError::EmptyComponent(_)));
        assert!(matches!(err("a/b/"), PathError::EmptyComponent(_)));
        assert!(matches!(err("a\0b"), PathError::Nul(_)));
        assert!(matches!(err(&"x".repeat(256)), PathError::TooLong(_)));
        ok(&"x".repeat(255));
    }

    #[test]
    fn test_sanitize_component() {
        assert_eq!(sanitize_component("normal.txt"), "normal.txt");
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("."), "_");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(sanitize_component("a/b"), "a_b");
        assert_eq!(sanitize_component("a\\b"), "a_b");
        assert_eq!(sanitize_component("a\0b"), "a_b");
        assert_eq!(sanitize_component("a\nb"), "a_b");
        let long = format!("{}.mkv", "y".repeat(300));
        let s = sanitize_component(&long);
        assert!(s.len() <= MAX_COMPONENT_LEN);
        assert!(s.ends_with(".mkv"));
        let long_unicode = "я".repeat(200);
        let s = sanitize_component(&long_unicode);
        assert!(s.len() <= MAX_COMPONENT_LEN);
    }

    #[test]
    fn test_sanitized_torrent_path_stays_relative() {
        let p = sanitized_torrent_path(["..", "..", "etc", "passwd"].map(Cow::Borrowed));
        assert_eq!(p, PathBuf::from("_/_/etc/passwd"));
        let p = sanitized_torrent_path(["/abs", "x"].map(Cow::Borrowed));
        assert_eq!(p, PathBuf::from("_abs/x"));
        let p = sanitized_torrent_path(Vec::<Cow<'_, str>>::new());
        assert_eq!(p, PathBuf::from("_"));
        // Whatever the input, the result validates as a relative path.
        for input in [
            vec!["..", "a"],
            vec!["", "b"],
            vec![".", "c"],
            vec!["d\0", "e"],
        ] {
            let p = sanitized_torrent_path(input.into_iter().map(Cow::Borrowed));
            validate_relative_path(&p).unwrap();
        }
    }

    #[test]
    fn test_suffix_naming() {
        let p = Path::new("dir/movie.mkv");
        assert_eq!(
            decorated_path(p, Some(".!qB"), false),
            PathBuf::from("dir/movie.mkv.!qB")
        );
        assert_eq!(
            decorated_path(p, None, false),
            PathBuf::from("dir/movie.mkv")
        );
        assert_eq!(
            decorated_path(p, Some(""), false),
            PathBuf::from("dir/movie.mkv")
        );
        assert_eq!(
            decorated_path(Path::new("file"), Some(".part"), false),
            PathBuf::from("file.part")
        );
        validate_suffix(".!qB").unwrap();
        assert!(validate_suffix("").is_err());
        assert!(validate_suffix("a/b").is_err());
    }

    #[test]
    fn test_unwanted_folder_mapping() {
        assert_eq!(
            decorated_path(Path::new("dir/sub/a.bin"), None, true),
            PathBuf::from("dir/sub/.unwanted/a.bin")
        );
        assert_eq!(
            decorated_path(Path::new("a.bin"), None, true),
            PathBuf::from(".unwanted/a.bin")
        );
        // Suffix first, then folder.
        assert_eq!(
            decorated_path(Path::new("dir/a.bin"), Some(".!qB"), true),
            PathBuf::from("dir/.unwanted/a.bin.!qB")
        );
        assert!(is_in_unwanted_folder(Path::new("dir/.unwanted/a.bin")));
        assert!(!is_in_unwanted_folder(Path::new("dir/a.bin")));
        assert!(!is_in_unwanted_folder(Path::new(".unwanted")));
    }

    #[test]
    fn test_path_variants() {
        let v = path_variants(Path::new("d/f"), Some(".!qB"));
        assert_eq!(
            v,
            vec![
                PathBuf::from("d/f"),
                PathBuf::from("d/f.!qB"),
                PathBuf::from("d/.unwanted/f"),
                PathBuf::from("d/.unwanted/f.!qB"),
            ]
        );
        let v = path_variants(Path::new("f"), None);
        assert_eq!(v, vec![PathBuf::from("f"), PathBuf::from(".unwanted/f")]);
    }
}
