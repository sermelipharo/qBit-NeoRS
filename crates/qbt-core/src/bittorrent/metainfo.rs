//! Torrent metadata: parsing .torrent files (through the engine's validating
//! parser plus a bounded scan for fields it ignores), magnet links and bare
//! info hashes, building magnet URIs and exporting .torrent files.
//!
//! Everything here treats its input as hostile: file paths are sanitised
//! component by component so that no path can escape the save folder.

use std::collections::HashSet;
use std::net::SocketAddr;

use super::bencode::{self, Limits, RawOrValue, Value};
use super::paths;
use crate::model::TrackerTierEntry;

#[derive(Clone, Debug, PartialEq)]
pub struct MetaFile {
    /// Original (sanitised) relative path, `/`-separated.
    pub path: String,
    pub size: u64,
    pub offset: u64,
    pub first_piece: u32,
    /// Inclusive.
    pub last_piece: u32,
    /// Index of the file in the engine's file list (which also contains
    /// BEP 47 padding files; those are hidden here like in qBittorrent).
    pub engine_index: u32,
}

/// Parsed, sanitised torrent metadata. Kept in memory per torrent, so it
/// holds no piece hashes or raw info bytes (those are read from
/// `BT_backup/<id>.torrent` when needed).
#[derive(Clone, Debug, PartialEq)]
pub struct TorrentMeta {
    /// The engine's (wire) identity: the v1 info hash, or for a pure v2
    /// torrent the SHA-256 info hash truncated to 20 bytes. The torrent id
    /// is [`TorrentMeta::id`].
    pub info_hash: [u8; 20],
    /// SHA-256 info hash of v2 and hybrid torrents.
    pub info_hash_v2: Option<[u8; 32]>,
    pub name: String,
    pub piece_length: u32,
    pub pieces: u32,
    pub total_size: u64,
    pub private: bool,
    pub files: Vec<MetaFile>,
    /// Number of files including padding files.
    pub engine_file_count: u32,
    pub comment: String,
    pub created_by: String,
    /// Unix seconds, -1 unknown.
    pub creation_date: i64,
    pub trackers: Vec<TrackerTierEntry>,
    pub url_seeds: Vec<String>,
    /// A pure v2 torrent whose .torrent lacks the piece layers of some
    /// files: like libtorrent, the engine starts it at once and fetches
    /// them from peers (the files without one wait for theirs); the
    /// .torrent is saved with each one fetched.
    pub missing_piece_layers: bool,
}

impl TorrentMeta {
    /// qBittorrent's `TorrentID` (libtorrent `info_hash_t::get_best()`):
    /// the v2 hash truncated to 20 bytes whenever the torrent has one
    /// (pure v2 and hybrid), the v1 hash otherwise.
    pub fn id(&self) -> String {
        match &self.info_hash_v2 {
            Some(v2) => hex(&v2[..20]),
            None => hex(&self.info_hash),
        }
    }

    /// The engine's identity (v1 hash, truncated v2 hash for pure v2): the
    /// name of the engine's fast-resume files.
    pub fn engine_hash_hex(&self) -> String {
        hex(&self.info_hash)
    }

    /// The other id of a hybrid torrent (its v1 hash), under which it was
    /// known before qBittorrent's id rule applied and which btih-only
    /// magnets use (`m_hybridTorrentsByAltID`).
    pub fn alt_id(&self) -> Option<String> {
        self.is_hybrid().then(|| hex(&self.info_hash))
    }

    /// Both a v1 and a v2 hash.
    pub fn is_hybrid(&self) -> bool {
        self.info_hash_v2.is_some() && !self.is_pure_v2()
    }

    /// "Info hash v1" as qBittorrent shows it: empty for a pure v2 torrent.
    pub fn info_hash_v1_hex(&self) -> String {
        if self.is_pure_v2() { String::new() } else { hex(&self.info_hash) }
    }

    /// "Info hash v2": empty for a v1-only torrent.
    pub fn info_hash_v2_hex(&self) -> String {
        self.info_hash_v2.map(|h| hex(&h)).unwrap_or_default()
    }

    /// A v2-only torrent (its id is the truncated v2 hash).
    pub fn is_pure_v2(&self) -> bool {
        self.info_hash_v2.is_some_and(|v2| v2[..20] == self.info_hash)
    }

    pub fn file_paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }
}

pub struct ParsedTorrent {
    pub meta: TorrentMeta,
    /// Raw bencoded `info` dictionary.
    pub info_bytes: Vec<u8>,
}

pub fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

/// A v2 info hash (64 hex characters).
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.as_bytes();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// The 20-byte id of a v2-only torrent: its v2 hash truncated (BEP 52).
pub fn truncate_v2(v2: &[u8; 32]) -> [u8; 20] {
    let mut out = [0u8; 20];
    out.copy_from_slice(&v2[..20]);
    out
}

pub fn parse_hex20(s: &str) -> Option<[u8; 20]> {
    let s = s.as_bytes();
    if s.len() != 40 {
        return None;
    }
    let mut out = [0u8; 20];
    for (i, chunk) in s.chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

pub fn parse_base32_20(s: &str) -> Option<[u8; 20]> {
    let s = s.as_bytes();
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 20];
    let mut acc: u64 = 0;
    let mut bits = 0;
    let mut n = 0;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        } as u64;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out[n] = (acc >> bits) as u8;
            n += 1;
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A bare v1 info hash (40 hex or 32 base32 characters).
pub fn parse_bare_hash(s: &str) -> Option<[u8; 20]> {
    parse_hex20(s).or_else(|| parse_base32_20(s))
}

pub fn is_v2_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn clean_text(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect::<String>().trim().to_string()
}

/// Single-line text (names, "created by"): no control characters at all,
/// so a name cannot forge extra lines in logs, mails or the UI; line
/// breaks and tabs become spaces.
fn clean_line(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Parses and validates a .torrent file.
pub fn parse_torrent(bytes: &[u8], limits: Limits) -> Result<ParsedTorrent, String> {
    bencode::validate(bytes, limits).map_err(|e| format!("invalid torrent file: {e}"))?;
    let (is, ie) = bencode::find_top_level_span(bytes, b"info", limits).ok_or("torrent has no info dictionary")?;
    let info_bytes = bytes[is..ie].to_vec();
    let parsed = librqbit::torrent_from_bytes(bytes).map_err(|e| format!("invalid torrent file: {e}"))?;
    let info_hash = parsed.info_hash.0;
    let info_hash_v2 = parsed.info_hash_v2.map(|h| h.0);
    let comment = parsed.comment.as_ref().map(|b| clean_text(&lossy(b.as_ref()))).unwrap_or_default();
    let created_by = parsed.created_by.as_ref().map(|b| clean_line(&lossy(b.as_ref()))).unwrap_or_default();
    let creation_date = parsed.creation_date.map(|d| d.min(i64::MAX as usize) as i64).unwrap_or(-1);

    let mut trackers = Vec::new();
    let mut seen = HashSet::new();
    if parsed.announce_list.iter().flatten().next().is_some() {
        for (tier, list) in parsed.announce_list.iter().enumerate() {
            for url in list {
                let url = lossy(url.as_ref()).trim().to_string();
                if !url.is_empty() && seen.insert(url.clone()) {
                    trackers.push(TrackerTierEntry { url, tier: tier as i64 });
                }
            }
        }
    } else if let Some(a) = &parsed.announce {
        let url = lossy(a.as_ref()).trim().to_string();
        if !url.is_empty() {
            trackers.push(TrackerTierEntry { url, tier: 0 });
        }
    }

    let piece_layers = parsed.piece_layers.as_ref().map(|l| l.0.as_ref());
    let info = parsed.info.data.validate().map_err(|e| format!("invalid torrent metadata: {e}"))?;
    // Missing piece layers (pure v2) are fetched from peers like
    // libtorrent does; layers that are present must be valid.
    let missing_piece_layers =
        !info.missing_piece_layers(piece_layers).map_err(|e| format!("invalid torrent metadata: {e}"))?.is_empty();
    let private = info.info().private;
    let piece_length = info.info().piece_length;
    let pieces = info.lengths().total_pieces();
    let total_size = info.lengths().total_length();
    let name = info.name().map(|n| clean_line(&n)).filter(|n| !n.is_empty()).unwrap_or_else(|| hex(&info_hash));

    // Multi-file torrents keep their files under a folder named after the
    // torrent (the engine's file names do not include it).
    let multi_file = !info.is_single_file();
    let root = paths::sanitize_component(&name);
    let mut files = Vec::new();
    let mut used = HashSet::new();
    let mut engine_file_count = 0u32;
    for (idx, fd) in info.iter_file_details_ext().enumerate() {
        engine_file_count += 1;
        if fd.details.attrs().padding {
            continue;
        }
        let comps: Vec<String> = fd.details.filename.iter_components().map(|c| c.into_owned()).collect();
        let mut path = paths::sanitize_components(comps.iter().map(String::as_str));
        if multi_file {
            path = format!("{root}/{path}");
        }
        if !used.insert(path.clone()) {
            // Sanitising made two paths equal: keep them apart.
            path = format!("{path}_{idx}");
            used.insert(path.clone());
        }
        let first = fd.pieces.start;
        let last = fd.pieces.end.max(first + 1) - 1;
        files.push(MetaFile {
            path,
            size: fd.details.len,
            offset: fd.offset,
            first_piece: first.min(pieces.saturating_sub(1)),
            last_piece: last.min(pieces.saturating_sub(1)),
            engine_index: idx as u32,
        });
    }
    if files.is_empty() {
        return Err("torrent has no files".into());
    }

    let url_seeds = url_list(bytes, limits);

    Ok(ParsedTorrent {
        meta: TorrentMeta {
            info_hash,
            info_hash_v2,
            name,
            piece_length,
            pieces,
            total_size,
            private,
            files,
            engine_file_count,
            comment,
            created_by,
            creation_date,
            trackers,
            url_seeds,
            missing_piece_layers,
        },
        info_bytes,
    })
}

/// BEP 19 `url-list` (string or list of strings).
fn url_list(bytes: &[u8], limits: Limits) -> Vec<String> {
    let Some((s, e)) = bencode::find_top_level_span(bytes, b"url-list", limits) else {
        return Vec::new();
    };
    let Ok(v) = bencode::decode(&bytes[s..e], limits) else { return Vec::new() };
    let mut out: Vec<String> = match v {
        Value::Bytes(b) => vec![lossy(&b)],
        Value::List(l) => l.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    out.retain(|u| is_web_url(u));
    out.dedup();
    out.truncate(MAX_WEB_SEEDS);
    out
}

/// Web seeds kept per torrent (the engine uses at most this many too).
pub const MAX_WEB_SEEDS: usize = 100;
/// Longest web seed URL accepted.
const MAX_WEB_URL_LEN: usize = 4096;

/// An http(s)/ftp URL without whitespace or control characters (they would
/// be stripped by URL parsing and could forge log lines).
pub fn is_web_url(u: &str) -> bool {
    let u = u.trim();
    let lower = u.to_ascii_lowercase();
    u.len() <= MAX_WEB_URL_LEN
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
        && ["http://", "https://", "ftp://"].iter().any(|p| lower.starts_with(p) && lower.len() > p.len())
}

/// Tracker URL sanity check used by the edit/add operations.
pub fn is_valid_tracker_url(u: &str) -> bool {
    let u = u.trim();
    let Some((scheme, rest)) = u.split_once("://") else { return false };
    !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        && !rest.is_empty()
        && !rest.starts_with('/')
        && !u.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Builds a .torrent file from the raw info dictionary (and the raw BEP 52
/// `piece layers` of v2 torrents) and the current trackers, web seeds and
/// descriptive fields.
pub fn build_torrent_file(
    info_bytes: &[u8],
    piece_layers: Option<&[u8]>,
    trackers: &[TrackerTierEntry],
    url_seeds: &[String],
    comment: &str,
    created_by: &str,
    creation_date: i64,
) -> Vec<u8> {
    let mut entries: Vec<(&str, RawOrValue<'_>)> = vec![("info", RawOrValue::Raw(info_bytes))];
    if let Some(layers) = piece_layers {
        entries.push(("piece layers", RawOrValue::Raw(layers)));
    }
    if let Some(first) = trackers.first() {
        entries.push(("announce", RawOrValue::Value(Value::Bytes(first.url.clone().into_bytes()))));
        let mut tiers: Vec<(i64, Vec<Value>)> = Vec::new();
        let mut sorted: Vec<&TrackerTierEntry> = trackers.iter().collect();
        sorted.sort_by_key(|t| t.tier);
        for t in sorted {
            match tiers.last_mut() {
                Some((tier, list)) if *tier == t.tier => list.push(Value::Bytes(t.url.clone().into_bytes())),
                _ => tiers.push((t.tier, vec![Value::Bytes(t.url.clone().into_bytes())])),
            }
        }
        entries.push((
            "announce-list",
            RawOrValue::Value(Value::List(tiers.into_iter().map(|(_, l)| Value::List(l)).collect())),
        ));
    }
    if !url_seeds.is_empty() {
        entries.push((
            "url-list",
            RawOrValue::Value(Value::List(url_seeds.iter().map(|u| Value::Bytes(u.clone().into_bytes())).collect())),
        ));
    }
    if !comment.is_empty() {
        entries.push(("comment", RawOrValue::Value(Value::Bytes(comment.as_bytes().to_vec()))));
    }
    if !created_by.is_empty() {
        entries.push(("created by", RawOrValue::Value(Value::Bytes(created_by.as_bytes().to_vec()))));
    }
    if creation_date > 0 {
        entries.push(("creation date", RawOrValue::Value(Value::Int(creation_date))));
    }
    let mut out = Vec::with_capacity(info_bytes.len() + 256);
    bencode::encode_dict(entries, &mut out);
    out
}

// ---- Magnet links ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Default)]
pub struct MagnetLink {
    /// The torrent id (qBittorrent's rule): the v2 hash truncated to 20
    /// bytes when the link has one, the v1 hash otherwise.
    pub info_hash: [u8; 20],
    /// `xt=urn:btih:` (v1 hash).
    pub info_hash_v1: Option<[u8; 20]>,
    /// `xt=urn:btmh:1220…` (BEP 52 v2 hash).
    pub info_hash_v2: Option<[u8; 32]>,
    pub name: String,
    pub trackers: Vec<TrackerTierEntry>,
    pub url_seeds: Vec<String>,
    pub peers: Vec<SocketAddr>,
}

pub fn pct_decode(s: &str, plus_as_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = (b[i + 1] as char).to_digit(16);
                let l = (b[i + 2] as char).to_digit(16);
                match (h, l) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `QUrl::toPercentEncoding`: everything except unreserved characters.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Parses a `magnet:` URI (BEP 9) or a bare info hash (v1, or 64 hex
/// characters for v2). A link with a v2 hash is identified by the
/// truncated v2 hash (with or without a v1 hash, like qBittorrent's
/// `TorrentID`), a v1-only link by the v1 hash. The `so` parameter is
/// deliberately ignored (it is applied as file priorities).
pub fn parse_magnet(input: &str) -> Result<MagnetLink, String> {
    let input = input.trim();
    if let Some(h) = parse_bare_hash(input) {
        return Ok(MagnetLink { info_hash: h, info_hash_v1: Some(h), ..Default::default() });
    }
    if let Some(v2) = parse_hex32(input) {
        return Ok(MagnetLink { info_hash: truncate_v2(&v2), info_hash_v2: Some(v2), ..Default::default() });
    }
    let lower = input.get(..7).map(str::to_ascii_lowercase);
    if lower.as_deref() != Some("magnet:") {
        return Err("not a magnet link".into());
    }
    let query = input[7..].trim_start_matches('?');
    let mut link = MagnetLink::default();
    let mut tier = 0i64;
    for pair in query.split('&').take(4096) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let key = k.to_ascii_lowercase();
        // Indexed keys (`xt.1`, `tr.2`) are accepted.
        let key = key.split('.').next().unwrap_or("");
        match key {
            "xt" => {
                let v = pct_decode(v, false);
                let lv = v.to_ascii_lowercase();
                if let Some(h) = lv.strip_prefix("urn:btih:") {
                    let h = &v[v.len() - h.len()..];
                    link.info_hash_v1 = Some(parse_bare_hash(h).ok_or("invalid info hash in magnet link")?);
                } else if let Some(h) = lv.strip_prefix("urn:btmh:") {
                    // Multihash: 0x12 (SHA-256), 0x20 (32 bytes).
                    let v2 = h
                        .strip_prefix("1220")
                        .and_then(parse_hex32)
                        .ok_or("invalid v2 info hash in magnet link")?;
                    link.info_hash_v2 = Some(v2);
                }
            }
            "dn" => link.name = clean_line(&pct_decode(v, true)),
            "tr" => {
                let url = pct_decode(v, false).trim().to_string();
                if !url.is_empty() && !link.trackers.iter().any(|t| t.url == url) && link.trackers.len() < 1000 {
                    link.trackers.push(TrackerTierEntry { url, tier });
                    tier += 1;
                }
            }
            "ws" => {
                let url = pct_decode(v, false).trim().to_string();
                if is_web_url(&url) && !link.url_seeds.contains(&url) && link.url_seeds.len() < MAX_WEB_SEEDS {
                    link.url_seeds.push(url);
                }
            }
            "x" if k.eq_ignore_ascii_case("x.pe") => {
                if let Some(a) = parse_peer_address(&pct_decode(v, false))
                    && link.peers.len() < 1000
                {
                    link.peers.push(a);
                }
            }
            _ => {}
        }
    }
    link.info_hash = match (link.info_hash_v1, link.info_hash_v2) {
        (_, Some(v2)) => truncate_v2(&v2),
        (Some(v1), None) => v1,
        (None, None) => return Err("magnet link has no info hash".into()),
    };
    Ok(link)
}

/// `PeerAddress::parse`: `ip:port` or `[ipv6]:port` with a non-zero port.
pub fn parse_peer_address(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    let addr: SocketAddr = if s.starts_with('[') {
        s.parse().ok()?
    } else {
        let (ip, port) = s.split_once(':')?;
        if port.contains(':') {
            return None;
        }
        let ip: std::net::Ipv4Addr = ip.parse().ok()?;
        SocketAddr::new(ip.into(), port.parse().ok()?)
    };
    (addr.port() != 0).then_some(addr)
}

/// `TorrentImpl::createMagnetURI`.
/// A magnet URI with `xt=urn:btih:` for the v1 hash and `xt=urn:btmh:1220`
/// for the v2 hash (either may be empty, like libtorrent's `make_magnet_uri`).
pub fn build_magnet(
    v1: &str,
    v2: &str,
    name: &str,
    total_size: Option<u64>,
    trackers: &[TrackerTierEntry],
    url_seeds: &[String],
) -> String {
    let mut s = String::from("magnet:");
    let mut xt = Vec::new();
    if !v1.is_empty() {
        xt.push(format!("xt=urn:btih:{v1}"));
    }
    if !v2.is_empty() {
        xt.push(format!("xt=urn:btmh:1220{v2}"));
    }
    s.push('?');
    s.push_str(&xt.join("&"));
    // A torrent without a name is shown by its id (for a v2-only torrent
    // the truncated v2 hash): no `dn` then.
    let id = if v1.is_empty() { v2.get(..40).unwrap_or(v2) } else { v1 };
    if name != id && name != v2 && !name.is_empty() {
        s.push_str("&dn=");
        s.push_str(&pct_encode(name));
    }
    if let Some(size) = total_size {
        s.push_str(&format!("&xl={size}"));
    }
    for t in trackers {
        s.push_str("&tr=");
        s.push_str(&pct_encode(&t.url));
    }
    for u in url_seeds {
        s.push_str("&ws=");
        s.push_str(&pct_encode(u));
    }
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a small single- or multi-file .torrent for tests (piece hashes
    /// are zero; only the structure matters).
    pub fn make_torrent(name: &str, files: &[(&[&str], u64)], piece_length: u64, extra: &[(&str, Value)]) -> Vec<u8> {
        use std::collections::BTreeMap;
        let total: u64 = files.iter().map(|f| f.1).sum();
        let pieces = total.div_ceil(piece_length).max(1);
        let mut info = BTreeMap::new();
        info.insert(b"name".to_vec(), Value::Bytes(name.as_bytes().to_vec()));
        info.insert(b"piece length".to_vec(), Value::Int(piece_length as i64));
        info.insert(b"pieces".to_vec(), Value::Bytes(vec![0u8; pieces as usize * 20]));
        if files.len() == 1 && files[0].0.is_empty() {
            info.insert(b"length".to_vec(), Value::Int(files[0].1 as i64));
        } else {
            let list = files
                .iter()
                .map(|(p, len)| {
                    let mut d = BTreeMap::new();
                    d.insert(b"length".to_vec(), Value::Int(*len as i64));
                    d.insert(
                        b"path".to_vec(),
                        Value::List(p.iter().map(|c| Value::Bytes(c.as_bytes().to_vec())).collect()),
                    );
                    Value::Dict(d)
                })
                .collect();
            info.insert(b"files".to_vec(), Value::List(list));
        }
        let mut top = BTreeMap::new();
        top.insert(b"info".to_vec(), Value::Dict(info));
        for (k, v) in extra {
            top.insert(k.as_bytes().to_vec(), v.clone());
        }
        let mut out = Vec::new();
        bencode::encode(&Value::Dict(top), &mut out);
        out
    }

    #[test]
    fn parses_multi_file_torrent() {
        let data = make_torrent(
            "Album",
            &[(&["01.mp3"], 100), (&["sub", "02.mp3"], 50)],
            64,
            &[
                ("announce", Value::Bytes(b"http://t1/announce".to_vec())),
                (
                    "announce-list",
                    Value::List(vec![
                        Value::List(vec![Value::Bytes(b"http://t1/announce".to_vec())]),
                        Value::List(vec![Value::Bytes(b"udp://t2:80".to_vec()), Value::Bytes(b"http://t1/announce".to_vec())]),
                    ]),
                ),
                ("url-list", Value::Bytes(b"http://seed/x".to_vec())),
                ("comment", Value::Bytes(b"hello".to_vec())),
                ("creation date", Value::Int(1700000000)),
            ],
        );
        let p = parse_torrent(&data, Limits::default()).unwrap();
        let m = &p.meta;
        assert_eq!(m.name, "Album");
        assert_eq!(m.files.len(), 2);
        assert_eq!(m.files[0].path, "Album/01.mp3");
        assert_eq!(m.files[1].path, "Album/sub/02.mp3");
        assert_eq!((m.files[0].first_piece, m.files[0].last_piece), (0, 1));
        assert_eq!((m.files[1].first_piece, m.files[1].last_piece), (1, 2));
        assert_eq!(m.total_size, 150);
        assert_eq!(m.pieces, 3);
        assert_eq!(m.trackers, vec![
            TrackerTierEntry { url: "http://t1/announce".into(), tier: 0 },
            TrackerTierEntry { url: "udp://t2:80".into(), tier: 1 },
        ]);
        assert_eq!(m.url_seeds, vec!["http://seed/x".to_string()]);
        assert_eq!(m.comment, "hello");
        assert_eq!(m.creation_date, 1700000000);
        assert!(p.info_bytes.starts_with(b"d5:files") && p.info_bytes.ends_with(b"e"));

        // Export keeps the info hash.
        let exported = build_torrent_file(&p.info_bytes, None, &m.trackers, &m.url_seeds, "c", "me", 5);
        let again = parse_torrent(&exported, Limits::default()).unwrap();
        assert_eq!(again.meta.info_hash, m.info_hash);
        assert_eq!(again.meta.trackers, m.trackers);
        assert_eq!(again.meta.comment, "c");
    }

    #[test]
    fn rejects_traversal_and_sanitizes_names() {
        let evil = make_torrent("x", &[(&["..", "etc", "passwd"], 10)], 16, &[]);
        assert!(parse_torrent(&evil, Limits::default()).is_err());
        let odd = make_torrent("x", &[(&["a:b", "c\nd"], 10)], 16, &[]);
        let p = parse_torrent(&odd, Limits::default()).unwrap();
        assert_eq!(p.meta.files[0].path, "x/a_b/c_d");
        assert!(parse_torrent(b"garbage", Limits::default()).is_err());
        assert!(parse_torrent(b"d4:infoi1ee", Limits::default()).is_err());
    }

    #[test]
    fn magnets_and_hashes() {
        let h = "0123456789abcdef0123456789abcdef01234567";
        let m = parse_magnet(&format!(
            "magnet:?xt=urn:btih:{h}&dn=My+Name%21&tr=http%3A%2F%2Ft%2Fa&tr=udp://u:1&ws=http://w/x&x.pe=1.2.3.4:5&so=0-99999999999"
        ))
        .unwrap();
        assert_eq!(hex(&m.info_hash), h);
        assert_eq!(m.name, "My Name!");
        // Names never carry line breaks (log line forging).
        let forged = parse_magnet(&format!("magnet:?xt=urn:btih:{h}&dn=a%0A12:00%20-%20fake%0D%09b%07")).unwrap();
        assert_eq!(forged.name, "a 12:00 - fake  b");
        assert_eq!(clean_text("line1\nline2\u{7}"), "line1\nline2");
        assert_eq!(m.trackers.len(), 2);
        assert_eq!(m.trackers[0].url, "http://t/a");
        assert_eq!(m.trackers[1].tier, 1);
        assert_eq!(m.url_seeds, vec!["http://w/x".to_string()]);
        assert_eq!(m.peers, vec!["1.2.3.4:5".parse().unwrap()]);
        assert_eq!(hex(&parse_magnet(h).unwrap().info_hash), h);
        let b32 = parse_bare_hash("AERUKZ4JVPG66AJDIVTYTK6N54ASGRLH").unwrap();
        assert_eq!(hex(&b32), h);
        assert!(parse_magnet("magnet:?dn=x").is_err());
        // v2: bare hash, v2-only and hybrid links.
        let v2 = "ab".repeat(32);
        let bare = parse_magnet(&v2).unwrap();
        assert_eq!(hex(&bare.info_hash), v2[..40]);
        assert_eq!(bare.info_hash_v2.map(|h| hex(&h)), Some(v2.clone()));
        let only = parse_magnet(&format!("magnet:?xt=urn:btmh:1220{v2}&dn=x")).unwrap();
        assert_eq!(hex(&only.info_hash), v2[..40]);
        let both = parse_magnet(&format!("magnet:?xt=urn:btmh:1220{v2}&xt=urn:btih:{h}")).unwrap();
        // qBittorrent's TorrentID: the truncated v2 hash whenever there is one.
        assert_eq!(hex(&both.info_hash), v2[..40]);
        assert_eq!(both.info_hash_v1.map(|h| hex(&h)).as_deref(), Some(h));
        assert_eq!(both.info_hash_v2, bare.info_hash_v2);
        assert!(parse_magnet(&format!("magnet:?xt=urn:btmh:1114{v2}")).is_err());
        assert!(parse_magnet("magnet:?xt=urn:btmh:1220abcd").is_err());
        assert_eq!(build_magnet("", &v2, "", None, &[], &[]), format!("magnet:?xt=urn:btmh:1220{v2}"));
        assert_eq!(
            build_magnet(h, &v2, "", None, &[], &[]),
            format!("magnet:?xt=urn:btih:{h}&xt=urn:btmh:1220{v2}")
        );
        assert!(parse_magnet("magnet:?xt=urn:btih:zz").is_err());
        let uri = build_magnet(h, "", "a b", Some(10), &m.trackers, &m.url_seeds);
        assert_eq!(
            uri,
            format!("magnet:?xt=urn:btih:{h}&dn=a%20b&xl=10&tr=http%3A%2F%2Ft%2Fa&tr=udp%3A%2F%2Fu%3A1&ws=http%3A%2F%2Fw%2Fx")
        );
        let back = parse_magnet(&uri).unwrap();
        assert_eq!(back.name, "a b");
        assert_eq!(back.trackers[0].url, "http://t/a");
    }

    #[test]
    fn peer_addresses() {
        assert!(parse_peer_address("1.2.3.4:80").is_some());
        assert!(parse_peer_address("[::1]:80").is_some());
        assert!(parse_peer_address("1.2.3.4:0").is_none());
        assert!(parse_peer_address("::1:80").is_none());
        assert!(parse_peer_address("host:80").is_none());
        assert!(is_valid_tracker_url("udp://tracker.example:80/announce"));
        assert!(!is_valid_tracker_url("not a url"));
        assert!(!is_valid_tracker_url("http:///x"));
    }

    #[test]
    fn web_urls_are_single_line_and_bounded() {
        assert!(is_web_url(" http://h/f "));
        assert!(!is_web_url("http://h/f\nforged log line"));
        assert!(!is_web_url("http://h/a b"));
        assert!(!is_web_url("http://h/\u{7}"));
        assert!(!is_web_url(&format!("http://h/{}", "a".repeat(5000))));
        assert!(!is_web_url("file:///etc/passwd"));
    }
}
