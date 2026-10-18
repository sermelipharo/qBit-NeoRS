//! BEP 19 web seeds ("url-list", GetRight style): maps a piece to HTTP range
//! requests over the torrent's files, fetches them and assembles the piece.
//!
//! URL rules (BEP 19 / libtorrent):
//! * single-file torrent: a URL ending in `/` gets the torrent `name`
//!   appended, otherwise the URL is the file itself;
//! * multi-file torrent: the URL is a directory; `name/` and the file path are
//!   appended (a missing trailing `/` is added).
//!
//! Path components are percent-encoded. Pad files (BEP 47) are never
//! requested; their bytes are zero.

use std::time::Duration;

/// File layout of a torrent, independent of the engine's metadata types.
#[derive(Clone, Debug)]
pub struct Layout {
    pub name: String,
    pub piece_length: u64,
    pub files: Vec<LayoutFile>,
    /// True for single-file torrents (no `files` list in the info dict).
    pub single_file: bool,
}

#[derive(Clone, Debug)]
pub struct LayoutFile {
    /// Path components inside the torrent (excluding the torrent name).
    pub path: Vec<String>,
    pub length: u64,
    pub is_pad: bool,
}

impl Layout {
    pub fn total_length(&self) -> u64 {
        self.files.iter().map(|f| f.length).sum()
    }

    pub fn piece_count(&self) -> u32 {
        self.total_length().div_ceil(self.piece_length.max(1)) as u32
    }

    pub fn piece_size(&self, piece: u32) -> u64 {
        let start = piece as u64 * self.piece_length;
        self.piece_length.min(self.total_length().saturating_sub(start))
    }
}

/// One HTTP range to fetch and where it goes inside the piece buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeRequest {
    pub url: String,
    pub start: u64,
    /// Inclusive end offset within the file.
    pub end: u64,
    pub piece_offset: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum WebSeedError {
    #[error("invalid web seed URL: {0}")]
    BadUrl(String),
    #[error("piece {0} is out of range")]
    BadPiece(u32),
    #[error("HTTP status {0}")]
    Status(u16),
    #[error("server is busy, retry after {0:?}")]
    RetryAfter(Duration),
    #[error("server returned {got} bytes, expected {expected}")]
    Length { got: u64, expected: u64 },
    #[error("network error: {0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, WebSeedError>;

/// Whether a path component is a dot segment. URLs have no way to express
/// a literal `.` or `..` segment (even `%2E%2E` is one), so a torrent with
/// such a component would point requests outside the seed's directory.
fn is_dot_segment(s: &str) -> bool {
    matches!(s, "" | "." | "..")
}

/// RFC 3986 unreserved characters stay; everything else is %-encoded.
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Full URL of file `index` for web seed `base`.
pub fn file_url(base: &str, layout: &Layout, index: usize) -> Result<String> {
    let parsed = url::Url::parse(base).map_err(|_| WebSeedError::BadUrl(base.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(WebSeedError::BadUrl(base.to_string()));
    }
    let file = layout.files.get(index).ok_or_else(|| WebSeedError::BadUrl(base.to_string()))?;
    let dots = is_dot_segment(&layout.name) || (!layout.single_file && file.path.iter().any(|c| is_dot_segment(c)));
    if dots && !(layout.single_file && !base.ends_with('/')) {
        return Err(WebSeedError::BadUrl("dot segment in the torrent's file path".to_string()));
    }
    if layout.single_file {
        return Ok(if base.ends_with('/') { format!("{base}{}", encode_component(&layout.name)) } else { base.to_string() });
    }
    let mut url = base.to_string();
    if !url.ends_with('/') {
        url.push('/');
    }
    url.push_str(&encode_component(&layout.name));
    for c in &file.path {
        url.push('/');
        url.push_str(&encode_component(c));
    }
    Ok(url)
}

/// HTTP ranges needed for `piece` from web seed `base`.
pub fn requests_for_piece(base: &str, layout: &Layout, piece: u32) -> Result<Vec<RangeRequest>> {
    if piece >= layout.piece_count() {
        return Err(WebSeedError::BadPiece(piece));
    }
    let piece_start = piece as u64 * layout.piece_length;
    let piece_end = piece_start + layout.piece_size(piece);
    let mut out = Vec::new();
    let mut file_start = 0u64;
    for (i, f) in layout.files.iter().enumerate() {
        let file_end = file_start + f.length;
        let lo = piece_start.max(file_start);
        let hi = piece_end.min(file_end);
        if lo < hi && !f.is_pad {
            out.push(RangeRequest {
                url: file_url(base, layout, i)?,
                start: lo - file_start,
                end: hi - file_start - 1,
                piece_offset: (lo - piece_start) as usize,
            });
        }
        if file_end >= piece_end {
            break;
        }
        file_start = file_end;
    }
    Ok(out)
}

/// Largest full (non-range) response read when a server ignores `Range`
/// (the bytes before our range are read and discarded, not kept).
const MAX_FULL_BODY: u64 = 64 * 1024 * 1024;

/// Reads `take` bytes of the body after skipping `skip` bytes; only the taken
/// bytes are kept. A 206 reply must not be longer than `take`.
async fn read_range(mut resp: reqwest::Response, skip: u64, take: u64) -> Result<Vec<u8>> {
    let partial = resp.status().as_u16() == 206;
    if let Some(len) = resp.content_length()
        && partial
        && len > take
    {
        return Err(WebSeedError::Length { got: len, expected: take });
    }
    let mut skipped = 0u64;
    let mut out: Vec<u8> = Vec::with_capacity(take.min(1024 * 1024) as usize);
    while let Some(chunk) = resp.chunk().await.map_err(|e| WebSeedError::Network(e.to_string()))? {
        let mut chunk = &chunk[..];
        if skipped < skip {
            let n = (skip - skipped).min(chunk.len() as u64);
            skipped += n;
            chunk = &chunk[n as usize..];
        }
        let room = (take as usize).saturating_sub(out.len());
        if chunk.len() > room {
            if !partial {
                // Full-file reply: keep only the part we need.
                out.extend_from_slice(&chunk[..room]);
                return Ok(out);
            }
            return Err(WebSeedError::Length { got: (out.len() + chunk.len()) as u64, expected: take });
        }
        out.extend_from_slice(chunk);
        if !partial && out.len() as u64 == take {
            return Ok(out);
        }
    }
    if skipped < skip {
        return Err(WebSeedError::Length { got: skipped, expected: skip + take });
    }
    Ok(out)
}

/// Downloads `piece` from web seed `base`. The caller verifies the hash.
pub async fn fetch_piece(client: &reqwest::Client, base: &str, layout: &Layout, piece: u32) -> Result<Vec<u8>> {
    let size = layout.piece_size(piece) as usize;
    let mut buf = vec![0u8; size];
    for r in requests_for_piece(base, layout, piece)? {
        let expected = r.end - r.start + 1;
        let resp = client
            .get(&r.url)
            .header("Range", format!("bytes={}-{}", r.start, r.end))
            .send()
            .await
            .map_err(|e| WebSeedError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        if status == 503 {
            let secs = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(60)
                .clamp(5, 3600);
            return Err(WebSeedError::RetryAfter(Duration::from_secs(secs)));
        }
        let (skip, take) = match status {
            206 => (0u64, expected),
            // Server ignored the range: take our slice of the full body
            // (only acceptable for small files; bounded by the length check).
            200 => (r.start, expected),
            s => return Err(WebSeedError::Status(s)),
        };
        // Never read more than what we need (hostile or misconfigured servers).
        if status == 200 && skip + take > MAX_FULL_BODY {
            return Err(WebSeedError::Status(200));
        }
        let body = read_range(resp, skip, take).await?;
        let got = body.len() as u64;
        if got != take {
            return Err(WebSeedError::Length { got, expected });
        }
        buf[r.piece_offset..r.piece_offset + body.len()].copy_from_slice(&body);
    }
    Ok(buf)
}

#[cfg(test)]
mod tests;
