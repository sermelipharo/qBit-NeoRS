//! WebUI static files (qBittorrent `WebApplication::sendWebUIFile`):
//! the bundled qBittorrent WebUI embedded at build time (pre-translated,
//! gzip-compressed) or an alternative WebUI folder on disk.
//!
//! Path handling is strict: the decoded request path may not contain `\`,
//! NUL, `.` or `..` segments; embedded files are looked up in a static table
//! (no filesystem access); alternative UI files must resolve (after
//! following symlinks) inside the canonical root, be regular files, have no
//! symlinked directory between them and the root, and be at most 10 MiB.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bytes::Bytes;
use parking_lot::Mutex;

use super::api::ApiError;
use super::translate;

pub struct Asset {
    /// `public/...` or `private/...`.
    pub path: &'static str,
    /// gzip-compressed content.
    pub gz: &'static [u8],
    /// Uncompressed length.
    pub len: usize,
    /// Contains `${LANG}`, `${CACHEID}` or `${LANGUAGE_OPTIONS}`.
    pub dynamic: bool,
}

include!(concat!(env!("OUT_DIR"), "/webui_assets.rs"));

/// Maximum size of a served file.
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
/// Upper bound of the alternative UI translation cache.
const ALT_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;
const PREFERENCES_VIEW: &str = "/views/preferences.html";

pub fn find(path: &str) -> Option<&'static Asset> {
    ASSETS.binary_search_by(|a| a.path.cmp(path)).ok().map(|i| &ASSETS[i])
}

/// MIME type from the file name.
pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=UTF-8",
        "js" | "mjs" => "text/javascript; charset=UTF-8",
        "css" => "text/css; charset=UTF-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=UTF-8",
        "xml" => "application/xml",
        "wasm" => "application/wasm",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// qBittorrent's `getCachingInterval`.
pub fn cache_control(mime: &str) -> &'static str {
    if mime.starts_with("image/") {
        "private, max-age=604800"
    } else if mime.starts_with("text/css") || mime.starts_with("text/javascript") {
        "private, max-age=43200"
    } else {
        "no-store"
    }
}

/// Decoded request path validation: `\`, NUL, `.`/`..` segments -> 400.
/// `/` maps to `/index.html`.
pub fn normalize_request_path(path: &str) -> Result<String, ApiError> {
    if path.contains('\\') || path.contains('\0') || !path.starts_with('/') {
        return Err(ApiError::bad_request());
    }
    if path.split('/').any(|seg| seg == "." || seg == "..") {
        return Err(ApiError::bad_request());
    }
    Ok(if path == "/" { "/index.html".to_owned() } else { path.to_owned() })
}

/// Values substituted at serve time.
pub struct Substitutions<'a> {
    /// First two letters of the locale (ASCII letters only, else "en").
    pub lang: &'a str,
    pub cache_id: &'a str,
}

pub fn lang_code(locale: &str) -> String {
    let two: String = locale.chars().take(2).collect();
    if two.len() == 2 && two.chars().all(|c| c.is_ascii_alphabetic()) { two.to_ascii_lowercase() } else { "en".into() }
}

/// Only the English UI is bundled.
fn language_options() -> &'static str {
    "<option value=\"en\">English</option>"
}

fn substitute(text: &str, rel_path: &str, subs: &Substitutions<'_>) -> String {
    let mut out = text.replace("${LANG}", subs.lang).replace("${CACHEID}", subs.cache_id);
    if rel_path.ends_with(PREFERENCES_VIEW) && rel_path.starts_with("private") {
        out = out.replace("${LANGUAGE_OPTIONS}", language_options());
    }
    out
}

/// A file ready to send.
#[derive(Debug)]
pub struct StaticFile {
    pub body: Bytes,
    /// `body` is gzip-compressed.
    pub gzipped: bool,
    pub mime: &'static str,
}

/// Cache of substituted embedded files (keyed by path, valid for one
/// language/cache-id combination).
#[derive(Default)]
pub struct EmbeddedCache {
    lang: String,
    files: HashMap<&'static str, Bytes>,
}

/// Serves a file of the bundled WebUI.
pub fn serve_embedded(
    rel: &str,
    has_session: bool,
    subs: &Substitutions<'_>,
    cache: &Mutex<EmbeddedCache>,
) -> Result<StaticFile, ApiError> {
    let private = format!("private{rel}");
    let public = format!("public{rel}");
    let asset = if has_session { find(&private).or_else(|| find(&public)) } else { find(&public) };
    let asset = asset.ok_or_else(ApiError::not_found)?;
    let mime = mime_for(asset.path);
    if !asset.dynamic {
        return Ok(StaticFile { body: Bytes::from_static(asset.gz), gzipped: true, mime });
    }
    {
        let c = cache.lock();
        if c.lang == subs.lang
            && let Some(b) = c.files.get(asset.path)
        {
            return Ok(StaticFile { body: b.clone(), gzipped: false, mime });
        }
    }
    let raw = super::http::gunzip(asset.gz, asset.len).ok_or_else(|| ApiError::internal("Corrupt embedded file"))?;
    let text = String::from_utf8_lossy(&raw);
    let body = Bytes::from(substitute(&text, asset.path, subs).into_bytes());
    let mut c = cache.lock();
    if c.lang != subs.lang {
        c.lang = subs.lang.to_owned();
        c.files.clear();
    }
    c.files.insert(asset.path, body.clone());
    Ok(StaticFile { body, gzipped: false, mime })
}

/// Cache of translated alternative-UI files: path -> (mtime, lang, body).
#[derive(Default)]
pub struct AltCache {
    files: HashMap<PathBuf, (SystemTime, String, Bytes)>,
    bytes: usize,
}

impl AltCache {
    pub fn clear(&mut self) {
        self.files.clear();
        self.bytes = 0;
    }
}

/// Resolves and validates a file of the alternative WebUI folder. Blocking
/// (filesystem); run it on a blocking thread.
pub fn serve_alternative(
    root: &Path,
    rel: &str,
    has_session: bool,
    subs: &Substitutions<'_>,
    cache: &Mutex<AltCache>,
) -> Result<StaticFile, ApiError> {
    let root = std::fs::canonicalize(root).map_err(|_| ApiError::not_found())?;
    let components: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    let candidate = |folder: &str| {
        let mut p = root.join(folder);
        for c in &components {
            p.push(c);
        }
        p
    };
    let private = candidate("private");
    let path = if has_session && std::fs::symlink_metadata(&private).is_ok() { private } else { candidate("public") };
    let folder_rel = if path.starts_with(root.join("private")) { "private" } else { "public" };

    let meta = std::fs::metadata(&path).map_err(|_| ApiError::not_found())?;
    if !meta.is_file() {
        return Err(ApiError::internal("Unacceptable file type, only regular file is allowed."));
    }
    // No symlinked directory between the file and the root.
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root {
            break;
        }
        match std::fs::symlink_metadata(d) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(ApiError::internal("Symlinks inside alternative UI folder are forbidden."));
            }
            Ok(_) => {}
            Err(_) => return Err(ApiError::not_found()),
        }
        dir = d.parent();
    }
    // Whatever the links, the target must stay inside the root.
    let real = std::fs::canonicalize(&path).map_err(|_| ApiError::not_found())?;
    if !real.starts_with(&root) {
        return Err(ApiError::internal("Symlinks inside alternative UI folder are forbidden."));
    }
    if meta.len() > MAX_FILE_SIZE {
        return Err(ApiError::internal("File size exceeds limit"));
    }
    let mime = mime_for(rel);
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    {
        let c = cache.lock();
        if let Some((t, lang, body)) = c.files.get(&real)
            && *t == mtime
            && lang == subs.lang
        {
            return Ok(StaticFile { body: body.clone(), gzipped: false, mime });
        }
    }
    let data = read_limited(&real, MAX_FILE_SIZE)?;
    if !translate::is_translatable(rel) {
        return Ok(StaticFile { body: Bytes::from(data), gzipped: false, mime });
    }
    let text = String::from_utf8_lossy(&data);
    let translated = translate::translate_placeholders(&text);
    let rel_path = format!("{folder_rel}{rel}");
    let body = Bytes::from(substitute(&translated, &rel_path, subs).into_bytes());
    let mut c = cache.lock();
    if c.bytes + body.len() > ALT_CACHE_MAX_BYTES {
        c.clear();
    }
    c.bytes += body.len();
    if let Some((_, _, old)) = c.files.insert(real, (mtime, subs.lang.to_owned(), body.clone())) {
        c.bytes = c.bytes.saturating_sub(old.len());
    }
    Ok(StaticFile { body, gzipped: false, mime })
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, ApiError> {
    use std::io::Read as _;
    let f = std::fs::File::open(path).map_err(|_| ApiError::not_found())?;
    let mut data = Vec::new();
    f.take(max + 1).read_to_end(&mut data).map_err(|e| ApiError::internal(e.to_string()))?;
    if data.len() as u64 > max {
        return Err(ApiError::internal("File size exceeds limit"));
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs() -> Substitutions<'static> {
        Substitutions { lang: "en", cache_id: "abc123" }
    }

    #[test]
    fn embedded_table_is_sorted_and_complete() {
        assert!(ASSETS.windows(2).all(|w| w[0].path < w[1].path));
        for path in ["public/index.html", "public/login.js", "public/app.css", "public/icon.svg", "private/index.html", "private/app.js"] {
            assert!(find(path).is_some(), "{path}");
        }
        assert!(find("nope").is_none());
    }

    #[test]
    fn embedded_files_are_served_compressed() {
        let cache = Mutex::new(EmbeddedCache::default());
        let f = serve_embedded("/index.html", true, &subs(), &cache).unwrap();
        assert!(f.gzipped);
        assert_eq!(f.mime, "text/html; charset=UTF-8");
        let text = String::from_utf8(crate::webui::http::gunzip(&f.body, MAX_FILE_SIZE as usize).unwrap()).unwrap();
        assert!(text.contains("app.js?v="));
        for a in ASSETS {
            assert!(!a.dynamic, "{}", a.path);
            if translate::is_translatable(a.path) {
                let raw = crate::webui::http::gunzip(a.gz, a.len).unwrap();
                let t = String::from_utf8_lossy(&raw);
                assert!(!t.contains(")QBT_TR[CONTEXT="), "{}", a.path);
                // No inline scripts or styles (the CSP forbids them).
                if a.path.ends_with(".html") {
                    assert!(!t.contains("<script>") && !t.contains("<style") && !t.contains(" style="), "{}", a.path);
                }
            }
        }
    }

    #[test]
    fn login_page_is_public_and_private_needs_session() {
        let cache = Mutex::new(EmbeddedCache::default());
        let public = serve_embedded("/index.html", false, &subs(), &cache).unwrap();
        let private = serve_embedded("/index.html", true, &subs(), &cache).unwrap();
        assert_ne!(public.body, private.body);
        assert!(serve_embedded("/app.js", false, &subs(), &cache).is_err());
        assert!(serve_embedded("/app.js", true, &subs(), &cache).is_ok());
        // Session falls back to public files.
        assert!(serve_embedded("/login.js", true, &subs(), &cache).is_ok());
        assert!(serve_embedded("/app.css", true, &subs(), &cache).is_ok());
    }

    #[test]
    fn dynamic_placeholders_are_substituted() {
        let text = "<html lang=\"${LANG}\"><script src=\"a.js?${CACHEID}\"></script>${LANGUAGE_OPTIONS}";
        assert_eq!(substitute(text, "public/index.html", &subs()), "<html lang=\"en\"><script src=\"a.js?abc123\"></script>${LANGUAGE_OPTIONS}");
        assert!(substitute(text, "private/views/preferences.html", &subs()).ends_with("<option value=\"en\">English</option>"));
    }

    #[test]
    fn request_path_validation() {
        assert_eq!(normalize_request_path("/").unwrap(), "/index.html");
        assert_eq!(normalize_request_path("/app.css").unwrap(), "/app.css");
        for bad in ["/../etc/passwd", "/a/./b", "/a\\b", "/a/..", "relative", "/a\0b", "/.."] {
            assert_eq!(normalize_request_path(bad).unwrap_err().status, http::StatusCode::BAD_REQUEST, "{bad}");
        }
    }

    #[test]
    fn lang_codes() {
        assert_eq!(lang_code("de_DE"), "de");
        assert_eq!(lang_code(""), "en");
        assert_eq!(lang_code("\">"), "en");
        assert_eq!(lang_code("x"), "en");
    }

    #[test]
    fn translation_markers() {
        use translate::translate_placeholders as t;
        assert_eq!(t("a QBT_TR(Hello)QBT_TR[CONTEXT=Ctx] b"), "a Hello b");
        assert_eq!(t("QBT_TR(Say \"hi\")QBT_TR[CONTEXT=X]"), "Say &#34;hi&#34;");
        assert_eq!(t("QBT_TR(f(x))QBT_TR[CONTEXT=X_1]"), "f(x)");
        assert_eq!(t("QBT_TR()QBT_TR[CONTEXT=X]"), "QBT_TR()QBT_TR[CONTEXT=X]");
        assert_eq!(t("QBT_TR(a)QBT_TR[CONTEXT=1x]"), "QBT_TR(a)QBT_TR[CONTEXT=1x]");
        assert_eq!(t("QBT_TR(a)QBT_TR[CONTEXT=X"), "QBT_TR(a)QBT_TR[CONTEXT=X");
        assert_eq!(t("x QBT_TR(a QBT_TR(b)QBT_TR[CONTEXT=C] y"), "x a QBT_TR(b y");
        assert_eq!(t("QBT_TR(✓)QBT_TR[CONTEXT=C]QBT_TR(b)QBT_TR[CONTEXT=D]"), "✓b");
        assert_eq!(t(""), "");
    }

    #[cfg(unix)]
    #[test]
    fn alternative_ui_hardening() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ui");
        std::fs::create_dir_all(root.join("public/sub")).unwrap();
        std::fs::create_dir_all(root.join("private")).unwrap();
        std::fs::write(root.join("public/index.html"), "<html lang=\"${LANG}\">QBT_TR(Login)QBT_TR[CONTEXT=L]</html>").unwrap();
        std::fs::write(root.join("private/index.html"), "private").unwrap();
        std::fs::write(root.join("public/sub/a.png"), [0u8, 1, 2]).unwrap();
        let outside = dir.path().join("secret.txt");
        std::fs::write(&outside, "secret").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("public/link.txt")).unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("public/dirlink")).unwrap();
        std::fs::create_dir_all(root.join("public/adir")).unwrap();
        let cache = Mutex::new(AltCache::default());
        let s = Substitutions { lang: "de", cache_id: "1" };

        let f = serve_alternative(&root, "/index.html", false, &s, &cache).unwrap();
        assert_eq!(&f.body[..], b"<html lang=\"de\">Login</html>");
        let f = serve_alternative(&root, "/index.html", true, &s, &cache).unwrap();
        assert_eq!(&f.body[..], b"private");
        assert_eq!(serve_alternative(&root, "/sub/a.png", false, &s, &cache).unwrap().body.len(), 3);
        assert_eq!(serve_alternative(&root, "/missing", false, &s, &cache).unwrap_err().status, http::StatusCode::NOT_FOUND);
        // Symlinked file escaping the root.
        assert_eq!(
            serve_alternative(&root, "/link.txt", false, &s, &cache).unwrap_err().status,
            http::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Symlinked directory.
        assert_eq!(
            serve_alternative(&root, "/dirlink/secret.txt", false, &s, &cache).unwrap_err().status,
            http::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Directories are not regular files.
        assert_eq!(
            serve_alternative(&root, "/adir", false, &s, &cache).unwrap_err().status,
            http::StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
