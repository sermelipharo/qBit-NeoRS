//! Path rules: sanitising names that come from torrent metadata, validating
//! user renames, content layout transforms, root folder detection, the
//! `.!qB` / `.unwanted` decorations and excluded-file-name wildcards.
//!
//! Torrent paths are always relative, `/`-separated strings. Nothing in this
//! module touches the file system.

use crate::model::ContentLayout;

pub const QB_EXT: &str = ".!qB";
pub const UNWANTED_FOLDER: &str = ".unwanted";

/// Maximum length of one path component in bytes (APFS/HFS+ limit).
const MAX_NAME_BYTES: usize = 255;

/// Makes one path component from untrusted metadata safe to use on disk:
/// separators, NUL/control characters and `:` are replaced by `_`, and the
/// special names `""`, `.`, `..` become `_`.
pub fn sanitize_component(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c == '/' || c == '\\' || c == ':' || (c as u32) < 32 || c as u32 == 127 { '_' } else { c })
        .collect();
    if out.is_empty() || out == "." || out == ".." {
        out = "_".to_string();
    }
    if out.len() > MAX_NAME_BYTES {
        let mut cut = MAX_NAME_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

/// Joins sanitised components into a relative path.
pub fn sanitize_components<'a>(components: impl IntoIterator<Item = &'a str>) -> String {
    components.into_iter().map(sanitize_component).collect::<Vec<_>>().join("/")
}

/// `Utils::Fs::isValidName` (non-Windows rules, plus `:` which macOS
/// reserves).
pub fn is_valid_name(name: &str) -> bool {
    !(name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_NAME_BYTES
        || name.chars().any(|c| (c as u32) < 32 || c as u32 == 127 || c == '/' || c == ':'))
}

/// A relative torrent path is valid when it is non-empty, not absolute and
/// every component is a valid name.
pub fn is_valid_relative_path(path: &str) -> bool {
    !path.is_empty() && !path.starts_with('/') && !path.contains('\\') && path.split('/').all(is_valid_name)
}

/// Normalises a user supplied relative path: trims, converts `\` to `/`,
/// drops duplicate separators.
pub fn normalize_user_path(path: &str) -> String {
    path.trim().replace('\\', "/").split('/').filter(|c| !c.is_empty()).collect::<Vec<_>>().join("/")
}

/// `Path::findRootFolder`: the common first component when every path has
/// at least two components.
pub fn find_root_folder<S: AsRef<str>>(paths: &[S]) -> Option<String> {
    let mut root: Option<&str> = None;
    for p in paths {
        let p = p.as_ref();
        let (first, rest) = p.split_once('/')?;
        if rest.is_empty() {
            return None;
        }
        match root {
            None => root = Some(first),
            Some(r) if r != first => return None,
            _ => {}
        }
    }
    root.map(str::to_string)
}

/// Removes the extension as qBittorrent's `Path::removedExtension` does,
/// including common double extensions (`.tar.gz`).
pub fn remove_extension(path: &str) -> String {
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let lower = file.to_ascii_lowercase();
    for double in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".tar.z"] {
        if lower.len() > double.len() && lower.ends_with(double) {
            return format!("{dir}{}", &file[..file.len() - double.len()]);
        }
    }
    // A leading dot (hidden file) is not an extension.
    let skip = file.chars().next().map(char::len_utf8).unwrap_or(0);
    match file[skip..].rfind('.') {
        Some(i) => format!("{dir}{}", &file[..i + skip]),
        None => path.to_string(),
    }
}

/// Applies a content layout to the metadata file paths
/// (`SessionImpl::addTorrent_impl`).
pub fn apply_content_layout(paths: &[String], layout: ContentLayout) -> Vec<String> {
    if layout == ContentLayout::Original || paths.is_empty() {
        return paths.to_vec();
    }
    let root = find_root_folder(paths);
    let original = if root.is_some() { ContentLayout::Subfolder } else { ContentLayout::NoSubfolder };
    if original == layout {
        return paths.to_vec();
    }
    match layout {
        ContentLayout::NoSubfolder => strip_root_folder(paths),
        _ => {
            let folder = remove_extension(&paths[0]);
            let folder = folder.rsplit('/').next().unwrap_or(&folder).to_string();
            let folder = sanitize_component(&folder);
            paths.iter().map(|p| format!("{folder}/{p}")).collect()
        }
    }
}

pub fn strip_root_folder(paths: &[String]) -> Vec<String> {
    match find_root_folder(paths) {
        Some(root) => paths.iter().map(|p| p[root.len() + 1..].to_string()).collect(),
        None => paths.to_vec(),
    }
}

/// `TorrentImpl::makeActualPath`: the on-disk relative path for a user path.
pub fn make_actual_path(user_path: &str, append_qb_ext: bool, unwanted_folder: bool) -> String {
    let mut p = user_path.to_string();
    if append_qb_ext {
        p.push_str(QB_EXT);
    }
    if unwanted_folder {
        p = match p.rfind('/') {
            Some(i) => format!("{}/{UNWANTED_FOLDER}/{}", &p[..i], &p[i + 1..]),
            None => format!("{UNWANTED_FOLDER}/{p}"),
        };
    }
    p
}

/// Joins an absolute base folder and a relative torrent path.
pub fn join(base: &str, rel: &str) -> String {
    if rel.is_empty() {
        return base.to_string();
    }
    if base.is_empty() {
        return rel.to_string();
    }
    if base.ends_with('/') { format!("{base}{rel}") } else { format!("{base}/{rel}") }
}

/// Resolves `path` against `base` unless it is absolute. `~` is expanded.
pub fn resolve(base: &str, path: &str) -> String {
    let path = path.trim();
    let expanded = expand_home(path);
    if expanded.starts_with('/') {
        clean(&expanded)
    } else {
        clean(&join(base, &expanded))
    }
}

pub fn expand_home(path: &str) -> String {
    if (path == "~" || path.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        return format!("{}{}", home.to_string_lossy(), &path[1..]);
    }
    path.to_string()
}

/// Lexically normalises an absolute path (removes `.`, resolves `..`,
/// collapses separators, strips a trailing `/`).
pub fn clean(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

/// `Utils::Fs::toValidPath`: replaces characters that are invalid in file
/// names (`: ? " * < > |` and control characters).
/// `.` / `..` (possible as category names) become `_` so a category can
/// never point outside its parent folder.
pub fn to_valid_path(name: &str) -> String {
    let s = name
        .chars()
        .map(|c| if matches!(c, ':' | '?' | '"' | '*' | '<' | '>' | '|') || (c as u32) < 32 { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string();
    if s.is_empty() || s == "." || s == ".." { "_".to_string() } else { s }
}

/// `Utils::Fs::toValidFileName` used for exported .torrent names.
pub fn to_valid_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '?' | '"' | '*' | '<' | '>' | '|') && (*c as u32) >= 32)
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() || s == "." || s == ".." { "torrent".to_string() } else { s }
}

// ---- Wildcards -------------------------------------------------------------

/// Matches `text` against a wildcard pattern (`*`, `?`, `[...]` with `!`/`^`
/// negation and ranges), anchored, case-insensitively. Iterative with
/// backtracking limited to the last `*`, so it cannot blow up.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next)) = match_class(&p, pi, t[ti]) {
                        if matched {
                            pi = next;
                            ti += 1;
                            continue;
                        }
                    } else if t[ti] == '[' {
                        // Unterminated class: literal '['.
                        pi += 1;
                        ti += 1;
                        continue;
                    }
                }
                c if c == t[ti] => {
                    pi += 1;
                    ti += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, st)) => {
                pi = sp + 1;
                ti = st + 1;
                star = Some((sp, st + 1));
            }
            None => return false,
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Returns (matched, index after the class) or None if the class is not
/// terminated.
fn match_class(p: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(p.get(i), Some('!') | Some('^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            if p[i] <= c && c <= p[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if p[i] == c {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

/// Excluded file names (4.8.1): a file is excluded if a pattern matches its
/// file name or the name of any ancestor folder.
pub fn is_excluded<S: AsRef<str>>(path: &str, patterns: &[S]) -> bool {
    let patterns: Vec<&str> = patterns.iter().map(|p| p.as_ref().trim()).filter(|p| !p.is_empty()).collect();
    if patterns.is_empty() {
        return false;
    }
    path.split('/').any(|component| patterns.iter().any(|p| wildcard_match(p, component)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn sanitizes_hostile_components() {
        assert_eq!(sanitize_component(".."), "_");
        assert_eq!(sanitize_component("."), "_");
        assert_eq!(sanitize_component(""), "_");
        assert_eq!(sanitize_component("a/b"), "a_b");
        assert_eq!(sanitize_component("a\\b"), "a_b");
        assert_eq!(sanitize_component("x\0y\n"), "x_y_");
        assert_eq!(sanitize_component("c:d"), "c_d");
        assert_eq!(sanitize_components(["..", "etc", "passwd"]), "_/etc/passwd");
        assert!(sanitize_component(&"é".repeat(300)).len() <= 255);
    }

    #[test]
    fn validates_user_paths() {
        assert!(is_valid_relative_path("a/b.txt"));
        assert!(!is_valid_relative_path("/etc/passwd"));
        assert!(!is_valid_relative_path("../x"));
        assert!(!is_valid_relative_path("a/./b"));
        assert!(!is_valid_relative_path("a//b"));
        assert!(!is_valid_relative_path("a\\..\\b"));
        assert!(!is_valid_relative_path("a\0b"));
        assert!(!is_valid_relative_path(""));
        assert_eq!(normalize_user_path(" a//b\\c/ "), "a/b/c");
    }

    #[test]
    fn root_folder_detection() {
        assert_eq!(find_root_folder(&v(&["r/a", "r/b/c"])), Some("r".into()));
        assert_eq!(find_root_folder(&v(&["r/a", "s/b"])), None);
        assert_eq!(find_root_folder(&v(&["a"])), None);
        assert_eq!(find_root_folder(&v(&["r/a", "b"])), None);
    }

    #[test]
    fn content_layout_transforms() {
        let multi = v(&["Album/01.mp3", "Album/02.mp3"]);
        assert_eq!(apply_content_layout(&multi, ContentLayout::Original), multi);
        assert_eq!(apply_content_layout(&multi, ContentLayout::Subfolder), multi);
        assert_eq!(apply_content_layout(&multi, ContentLayout::NoSubfolder), v(&["01.mp3", "02.mp3"]));
        let single = v(&["movie.mkv"]);
        assert_eq!(apply_content_layout(&single, ContentLayout::Subfolder), v(&["movie/movie.mkv"]));
        assert_eq!(apply_content_layout(&single, ContentLayout::NoSubfolder), single);
        let archive = v(&["x.tar.gz"]);
        assert_eq!(apply_content_layout(&archive, ContentLayout::Subfolder), v(&["x/x.tar.gz"]));
        let rootless = v(&["a.txt", "b/c.txt"]);
        assert_eq!(apply_content_layout(&rootless, ContentLayout::Subfolder), v(&["a/a.txt", "a/b/c.txt"]));
    }

    #[test]
    fn actual_path_decorations() {
        assert_eq!(make_actual_path("a/b.bin", true, false), "a/b.bin.!qB");
        assert_eq!(make_actual_path("a/b.bin", false, true), "a/.unwanted/b.bin");
        assert_eq!(make_actual_path("b.bin", true, true), ".unwanted/b.bin.!qB");
        assert_eq!(make_actual_path("b.bin", false, false), "b.bin");
    }

    #[test]
    fn path_resolution() {
        assert_eq!(resolve("/dl", "movies"), "/dl/movies");
        assert_eq!(resolve("/dl", "/abs/x/"), "/abs/x");
        assert_eq!(resolve("/dl", "a/../b"), "/dl/b");
        assert_eq!(clean("/a/./b//c/.."), "/a/b");
        assert_eq!(remove_extension("dir/file.txt"), "dir/file");
        assert_eq!(remove_extension(".hidden"), ".hidden");
        assert_eq!(remove_extension("noext"), "noext");
        assert_eq!(to_valid_path(".."), "_");
        assert_eq!(to_valid_path("a:b"), "a b");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.exe", "setup.EXE"));
        assert!(wildcard_match("readme.txt", "README.txt"));
        assert!(wildcard_match("?.txt", "a.txt"));
        assert!(!wildcard_match("?.txt", "aa.txt"));
        assert!(wildcard_match("readme[0-9].txt", "readme1.txt"));
        assert!(!wildcard_match("readme[0-9].txt", "readme10.txt"));
        assert!(wildcard_match("[!a]*", "b"));
        assert!(!wildcard_match("[!a]*", "a"));
        assert!(wildcard_match("*", ""));
        assert!(!wildcard_match("a*b", "a"));
        assert!(wildcard_match("a*b*c", "aXXbYYc"));
        assert!(wildcard_match("[", "["));
        assert!(!wildcard_match(&"*a".repeat(50), &"b".repeat(200)));
    }

    #[test]
    fn excluded_files_match_ancestors() {
        let patterns = v(&["*.nfo", "Sample", ""]);
        assert!(is_excluded("Movie/movie.nfo", &patterns));
        assert!(is_excluded("Movie/Sample/clip.mkv", &patterns));
        assert!(!is_excluded("Movie/movie.mkv", &patterns));
        assert!(!is_excluded("a.nfo", &Vec::<String>::new()));
    }
}
