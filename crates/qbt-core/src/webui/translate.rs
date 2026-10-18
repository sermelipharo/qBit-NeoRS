//! WebUI translation placeholders (qBittorrent `WebApplication::translateDocument`).
//!
//! qBittorrent's WebUI sources contain `QBT_TR(<source>)QBT_TR[CONTEXT=<ctx>]`
//! markers that the server replaces with the translation for the current
//! locale. Only the English source text is shipped, so every marker is
//! replaced by its source text (the fallback qBittorrent uses when a string
//! has no translation). Double quotes are escaped as `&#34;` exactly like
//! qBittorrent does so translated text can sit inside HTML attributes.
//!
//! This file is also compiled into `build.rs` (built-in assets are translated
//! at build time), so it must not depend on anything else in the crate.

const START: &str = "QBT_TR(";
const END: &str = ")QBT_TR";
const CONTEXT: &str = "[CONTEXT=";

/// Replaces every translation marker in `input` with its (escaped) source text.
pub fn translate_placeholders(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut copied = 0;
    let mut search_from = 0;
    while let Some(rel) = input[search_from..].find(START) {
        let start = search_from + rel;
        let src_begin = start + START.len();
        match match_marker(input, src_begin) {
            Some((src_end, match_end)) => {
                out.push_str(&input[copied..start]);
                for c in input[src_begin..src_end].chars() {
                    if c == '"' {
                        out.push_str("&#34;");
                    } else {
                        out.push(c);
                    }
                }
                copied = match_end;
                search_from = match_end;
            }
            None => search_from = start + 1,
        }
    }
    out.push_str(&input[copied..]);
    out
}

/// Equivalent of qBittorrent's regex
/// `QBT_TR\((([^\)]|\)(?!QBT_TR))+)\)QBT_TR\[CONTEXT=([a-zA-Z_][a-zA-Z0-9_]*)\]`
/// anchored at `src_begin` (just after `QBT_TR(`). Returns the end of the
/// source text and the end of the whole marker.
fn match_marker(input: &str, src_begin: usize) -> Option<(usize, usize)> {
    // The source text extends to the first ")QBT_TR" and must not be empty.
    let rel = input[src_begin..].find(END)?;
    if rel == 0 {
        return None;
    }
    let src_end = src_begin + rel;
    let after = &input[src_end + END.len()..];
    let ctx = after.strip_prefix(CONTEXT)?;
    let bytes = ctx.as_bytes();
    let first = *bytes.first()?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let ident_len = bytes.iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_').count();
    if bytes.get(ident_len) != Some(&b']') {
        return None;
    }
    let match_end = src_end + END.len() + CONTEXT.len() + ident_len + 1;
    Some((src_end, match_end))
}

/// File extensions whose content qBittorrent treats as text (MIME types that
/// inherit `text/plain`) and therefore translates.
pub fn is_translatable(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "html" | "htm" | "js" | "mjs" | "css" | "svg" | "txt" | "json" | "xml")
}

/// Runtime placeholders substituted when a file is served.
#[allow(dead_code)] // used by build.rs
pub const DYNAMIC_PLACEHOLDERS: [&str; 3] = ["${LANG}", "${CACHEID}", "${LANGUAGE_OPTIONS}"];

/// Whether `text` contains any placeholder that is substituted at serve time.
#[allow(dead_code)] // used by build.rs
pub fn has_dynamic_placeholders(text: &str) -> bool {
    DYNAMIC_PLACEHOLDERS.iter().any(|p| text.contains(p))
}
