//! qBittorrent's per-torrent resume data (`BT_backup/<id>.fastresume`):
//! libtorrent's bencoded resume file (`read_resume_data`) plus the `qBt-*`
//! keys of `BencodeResumeDataStorage`, and its `queue` file.
//!
//! [`LtResume::parse`] decodes it; [`LtResume::to_fast_resume`] builds our
//! JSON record and the engine's piece bitfield, mirroring
//! `BencodeResumeDataStorage::loadTorrentResumeData`.

use crate::bittorrent::bencode::{self, Limits, Value};
use crate::bittorrent::metainfo::{self, TorrentMeta};
use crate::bittorrent::resume::FastResume;
use crate::bittorrent::{categories, paths};
use crate::model::{FilePriority, TrackerTierEntry};

/// qBittorrent's suffix of incomplete files (`QB_EXT`).
pub const QB_EXT: &str = ".!qB";

/// The decoded resume file. Times are Unix seconds, durations seconds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LtResume {
    pub info_hash: Option<[u8; 20]>,
    pub info_hash2: Option<[u8; 32]>,
    /// libtorrent's `pieces`: one byte per piece, bit 0 = have.
    pub pieces: Option<Vec<u8>>,
    /// Per libtorrent file index (padding files included).
    pub file_priority: Vec<i64>,
    /// Per libtorrent file index; empty = not renamed.
    pub mapped_files: Vec<String>,
    /// Announce tiers.
    pub trackers: Vec<Vec<String>>,
    pub url_seeds: Vec<String>,
    pub http_seeds: Vec<String>,
    /// Where the data is (libtorrent's `save_path`).
    pub save_path: String,
    pub name: String,
    pub paused: bool,
    pub auto_managed: bool,
    pub sequential: bool,
    pub super_seeding: bool,
    pub seed_mode: bool,
    pub stop_when_ready: bool,
    pub upload_mode: bool,
    pub share_mode: bool,
    pub disable_dht: bool,
    pub disable_pex: bool,
    pub disable_lsd: bool,
    pub added_time: i64,
    pub completed_time: i64,
    pub last_seen_complete: i64,
    pub active_time: i64,
    pub finished_time: i64,
    pub seeding_time: i64,
    pub last_download: i64,
    pub last_upload: i64,
    pub total_uploaded: i64,
    pub total_downloaded: i64,
    /// Bytes/s, -1/0 = unlimited.
    pub upload_rate_limit: i64,
    pub download_rate_limit: i64,
    pub max_connections: i64,
    pub max_uploads: i64,
    /// Partially downloaded pieces (`unfinished`), not carried over.
    pub unfinished_pieces: usize,
    pub has_piece_priority: bool,
    pub peer_count: usize,
    pub libtorrent_version: String,
    // qBittorrent keys
    pub category: String,
    pub tags: Vec<String>,
    pub qbt_name: String,
    pub qbt_comment: Option<String>,
    pub qbt_save_path: String,
    pub qbt_download_path: String,
    pub ratio_limit: f64,
    pub seeding_time_limit: i64,
    pub inactive_seeding_time_limit: i64,
    pub share_limit_action: String,
    pub content_layout: String,
    pub stop_condition: String,
    pub seed_status: bool,
    pub first_last_piece_priority: bool,
    /// `qBt-queuePosition` of old versions (the `queue` file replaced it).
    pub queue_position: Option<i64>,
    pub has_ssl_parameters: bool,
}

fn bstr(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default()
}

fn int(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(Value::as_int).unwrap_or(default)
}

fn flag(v: Option<&Value>) -> bool {
    int(v, 0) != 0
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_list)
        .map(|l| l.iter().filter_map(Value::as_str).filter(|s| !s.trim().is_empty()).collect())
        .unwrap_or_default()
}

impl LtResume {
    pub fn parse(data: &[u8], limits: Limits) -> Result<Self, String> {
        let root = bencode::decode(data, limits).map_err(|e| format!("Cannot parse resume data: {e}"))?;
        if root.as_dict().is_none() {
            return Err("Cannot parse resume data: invalid format".into());
        }
        let g = |k: &str| root.get(k);
        let mut r = LtResume {
            info_hash: g("info-hash").and_then(Value::as_bytes).and_then(|b| b.try_into().ok()),
            info_hash2: g("info-hash2").and_then(Value::as_bytes).and_then(|b| b.try_into().ok()),
            pieces: g("pieces").and_then(Value::as_bytes).map(<[u8]>::to_vec),
            file_priority: g("file_priority")
                .and_then(Value::as_list)
                .map(|l| l.iter().map(|v| v.as_int().unwrap_or(4)).collect())
                .unwrap_or_default(),
            mapped_files: g("mapped_files")
                .and_then(Value::as_list)
                .map(|l| l.iter().map(|v| v.as_str().unwrap_or_default()).collect())
                .unwrap_or_default(),
            trackers: g("trackers")
                .and_then(Value::as_list)
                .map(|tiers| tiers.iter().map(|t| str_list(Some(t))).filter(|t| !t.is_empty()).collect())
                .unwrap_or_default(),
            url_seeds: str_list(g("url-list")),
            http_seeds: str_list(g("httpseeds")),
            save_path: bstr(g("save_path")),
            name: bstr(g("name")),
            paused: flag(g("paused")),
            auto_managed: flag(g("auto_managed")),
            sequential: flag(g("sequential_download")),
            super_seeding: flag(g("super_seeding")),
            seed_mode: flag(g("seed_mode")),
            stop_when_ready: flag(g("stop_when_ready")),
            upload_mode: flag(g("upload_mode")),
            share_mode: flag(g("share_mode")),
            disable_dht: flag(g("disable_dht")),
            disable_pex: flag(g("disable_pex")),
            disable_lsd: flag(g("disable_lsd")),
            added_time: int(g("added_time"), 0),
            completed_time: int(g("completed_time"), 0),
            last_seen_complete: int(g("last_seen_complete"), 0),
            active_time: int(g("active_time"), 0),
            finished_time: int(g("finished_time"), 0),
            seeding_time: int(g("seeding_time"), 0),
            last_download: int(g("last_download"), 0),
            last_upload: int(g("last_upload"), 0),
            total_uploaded: int(g("total_uploaded"), 0),
            total_downloaded: int(g("total_downloaded"), 0),
            upload_rate_limit: int(g("upload_rate_limit"), -1),
            download_rate_limit: int(g("download_rate_limit"), -1),
            max_connections: int(g("max_connections"), -1),
            max_uploads: int(g("max_uploads"), -1),
            unfinished_pieces: g("unfinished").and_then(Value::as_list).map(<[Value]>::len).unwrap_or(0),
            has_piece_priority: g("piece_priority").and_then(Value::as_bytes).is_some_and(|b| b.iter().any(|&p| p != 4 && p != 1)),
            peer_count: g("peers").and_then(Value::as_bytes).map(|b| b.len() / 6).unwrap_or(0)
                + g("peers6").and_then(Value::as_bytes).map(|b| b.len() / 18).unwrap_or(0),
            libtorrent_version: bstr(g("libtorrent-version")),
            category: bstr(g("qBt-category")),
            tags: str_list(g("qBt-tags")),
            qbt_name: bstr(g("qBt-name")),
            qbt_comment: g("qBt-comment").and_then(Value::as_str),
            qbt_save_path: bstr(g("qBt-savePath")),
            qbt_download_path: bstr(g("qBt-downloadPath")),
            ratio_limit: -2.0,
            seeding_time_limit: int(g("qBt-seedingTimeLimit"), -2),
            inactive_seeding_time_limit: int(g("qBt-inactiveSeedingTimeLimit"), -2),
            share_limit_action: bstr(g("qBt-shareLimitAction")),
            content_layout: String::new(),
            stop_condition: bstr(g("qBt-stopCondition")),
            seed_status: flag(g("qBt-seedStatus")),
            first_last_piece_priority: flag(g("qBt-firstLastPiecePriority")),
            queue_position: g("qBt-queuePosition").and_then(Value::as_int),
            has_ssl_parameters: ["qBt-sslCertificate", "qBt-sslPrivateKey", "qBt-sslDhParams"]
                .iter()
                .any(|k| g(k).and_then(Value::as_bytes).is_some_and(|b| !b.is_empty())),
        };
        // qBt-ratioLimit: a decimal string, or (older) an int x 1000.
        r.ratio_limit = match g("qBt-ratioLimit") {
            Some(Value::Bytes(b)) => String::from_utf8_lossy(b).trim().parse::<f64>().unwrap_or(-2.0),
            Some(Value::Int(i)) => *i as f64 / 1000.0,
            _ => -2.0,
        };
        // qBt-contentLayout, or the deprecated qBt-hasRootFolder.
        r.content_layout = match g("qBt-contentLayout") {
            Some(Value::Bytes(b)) => match String::from_utf8_lossy(b).as_ref() {
                "Subfolder" => "Subfolder".into(),
                "NoSubfolder" => "NoSubfolder".into(),
                _ => "Original".into(),
            },
            _ if g("qBt-hasRootFolder").and_then(Value::as_int) == Some(0) => "NoSubfolder".into(),
            _ => "Original".into(),
        };
        if r.save_path.is_empty() {
            return Err("Corrupted resume data: save_path is invalid".into());
        }
        Ok(r)
    }

    /// `torrentParams.stopped`: paused and not auto-managed.
    pub fn stopped(&self) -> bool {
        self.paused && !self.auto_managed
    }

    /// `TorrentOperatingMode::Forced` when neither paused nor auto-managed.
    pub fn forced(&self) -> bool {
        !self.paused && !self.auto_managed
    }

    /// The have bitfield when it can be trusted, or why it can't.
    pub fn have_pieces(&self, piece_count: u32) -> Result<Vec<bool>, String> {
        let Some(p) = &self.pieces else { return Err("no piece bitfield in the resume data".into()) };
        if p.len() != piece_count as usize {
            return Err(format!("piece bitfield has {} entries, the torrent has {piece_count} pieces", p.len()));
        }
        if self.seed_mode {
            return Err("added in seed mode (the data was never verified)".into());
        }
        Ok(p.iter().map(|b| b & 1 != 0).collect())
    }
}

/// The `queue` file: one torrent id per line, in queue order.
pub fn parse_queue(data: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(data)
        .lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| l.len() == 40 && l.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect()
}

/// Our record for a torrent with metadata. `have` is the bitfield (in
/// piece order) when it is claimed, used for `completed_bytes`.
pub fn to_fast_resume(r: &LtResume, id: &str, meta: Option<&TorrentMeta>, have: Option<&[bool]>, queue_position: i64) -> (FastResume, Vec<String>) {
    let mut notes = Vec::new();
    let auto_tmm = r.qbt_save_path.is_empty();
    let mut fr = FastResume {
        info_hash: id.to_owned(),
        name: r.qbt_name.clone(),
        category: r.category.clone(),
        tags: Vec::new(),
        auto_tmm,
        save_path: if auto_tmm { String::new() } else { r.qbt_save_path.clone() },
        download_path: if auto_tmm { String::new() } else { r.qbt_download_path.clone() },
        content_layout: r.content_layout.clone(),
        added_on: r.added_time,
        completed_on: if r.completed_time > 0 { r.completed_time } else { -1 },
        seen_complete: if r.last_seen_complete > 0 { r.last_seen_complete } else { -1 },
        all_time_download: r.total_downloaded.max(0),
        all_time_upload: r.total_uploaded.max(0),
        active_time: r.active_time.max(0),
        seeding_time: r.seeding_time.max(0),
        last_download: if r.last_download > 0 { r.last_download } else { -1 },
        last_upload: if r.last_upload > 0 { r.last_upload } else { -1 },
        ratio_limit: if r.ratio_limit < -2.0 || !r.ratio_limit.is_finite() { -1.0 } else { r.ratio_limit },
        seeding_time_limit: r.seeding_time_limit.max(-2),
        inactive_seeding_time_limit: r.inactive_seeding_time_limit.max(-2),
        share_limit_action: match r.share_limit_action.as_str() {
            "Stop" | "Remove" | "RemoveWithContent" | "EnableSuperSeeding" => r.share_limit_action.clone(),
            _ => "Default".into(),
        },
        download_limit: r.download_rate_limit.max(0),
        upload_limit: r.upload_rate_limit.max(0),
        stopped: r.stopped(),
        operating_mode: if r.forced() { "Forced".into() } else { "AutoManaged".into() },
        stop_condition: if r.stop_when_ready {
            "FilesChecked".into()
        } else {
            match r.stop_condition.as_str() {
                "MetadataReceived" | "FilesChecked" => r.stop_condition.clone(),
                _ => "None".into(),
            }
        },
        queue_position,
        sequential: r.sequential,
        first_last_piece_priority: r.first_last_piece_priority,
        super_seeding: r.super_seeding,
        seed_status: r.seed_status,
        location: r.save_path.clone(),
        disable_dht: r.disable_dht,
        disable_pex: r.disable_pex,
        disable_lsd: r.disable_lsd,
        ..Default::default()
    };
    if !r.category.is_empty() && !categories::is_valid_category_name(&r.category) {
        notes.push(format!("invalid category \"{}\" dropped", r.category));
        fr.category.clear();
    }
    for t in &r.tags {
        if categories::is_valid_tag(t) {
            if !fr.tags.contains(t) {
                fr.tags.push(t.clone());
            }
        } else {
            notes.push(format!("invalid tag \"{t}\" dropped"));
        }
    }
    for (tier, urls) in r.trackers.iter().enumerate() {
        for u in urls {
            let u = u.trim();
            if !fr.trackers.iter().any(|t| t.url == u) {
                fr.trackers.push(TrackerTierEntry { url: u.to_owned(), tier: tier as i64 });
            }
        }
    }
    for u in &r.url_seeds {
        let u = u.trim();
        if metainfo::is_web_url(u) && !fr.url_seeds.iter().any(|s| s == u) {
            fr.url_seeds.push(u.to_owned());
        }
    }
    // Everything came from the user's own client: trusted like local input.
    fr.trusted_urls = fr.trackers.iter().map(|t| t.url.clone()).chain(fr.url_seeds.iter().cloned()).collect();

    match meta {
        Some(m) => {
            fr.comment = r.qbt_comment.clone().filter(|c| *c != m.comment);
            let mut renamed = 0;
            fr.file_paths = m
                .files
                .iter()
                .map(|f| {
                    let mapped = r.mapped_files.get(f.engine_index as usize).map(String::as_str).unwrap_or("");
                    if mapped.is_empty() {
                        f.path.clone()
                    } else {
                        let p = mapped.strip_suffix(QB_EXT).unwrap_or(mapped);
                        let p = paths::normalize_user_path(p);
                        if p != f.path {
                            renamed += 1;
                        }
                        p
                    }
                })
                .collect();
            if !fr.file_paths.iter().all(|p| paths::is_valid_relative_path(p)) {
                notes.push("renamed file paths are invalid; the content layout is applied instead".into());
                fr.file_paths = paths::apply_content_layout(&m.file_paths(), crate::model::ContentLayout::Original);
                fr.file_paths = paths::apply_content_layout(
                    &fr.file_paths,
                    match fr.content_layout.as_str() {
                        "Subfolder" => crate::model::ContentLayout::Subfolder,
                        "NoSubfolder" => crate::model::ContentLayout::NoSubfolder,
                        _ => crate::model::ContentLayout::Original,
                    },
                );
            } else if renamed > 0 {
                notes.push(format!("{renamed} renamed file path(s)"));
            }
            fr.file_priorities = m
                .files
                .iter()
                .map(|f| {
                    let p = r.file_priority.get(f.engine_index as usize).copied().unwrap_or(4);
                    FilePriority::from_i32(p.clamp(0, 7) as i32).unwrap_or_default().as_i32()
                })
                .collect();
            if let Some(have) = have {
                fr.completed_bytes = completed_wanted_bytes(m, &fr.file_priorities, have);
            }
        }
        None => {
            fr.magnet_name = r.qbt_name.clone();
            fr.comment = r.qbt_comment.clone().filter(|c| !c.is_empty());
            if fr.magnet_name.is_empty() {
                fr.magnet_name = r.name.clone();
            }
            if let Some(v2) = r.info_hash2 {
                fr.info_hash_v2 = metainfo::hex(&v2);
            }
        }
    }
    (fr, notes)
}

/// Bytes of wanted files inside had pieces.
pub fn completed_wanted_bytes(m: &TorrentMeta, priorities: &[i32], have: &[bool]) -> i64 {
    let plen = u64::from(m.piece_length);
    let mut total: u64 = 0;
    for (f, p) in m.files.iter().zip(priorities) {
        if *p == 0 || f.size == 0 {
            continue;
        }
        for piece in f.first_piece..=f.last_piece {
            if !have.get(piece as usize).copied().unwrap_or(false) {
                continue;
            }
            let ps = u64::from(piece) * plen;
            let pe = (ps + plen).min(m.total_size);
            let s = ps.max(f.offset);
            let e = pe.min(f.offset + f.size);
            if e > s {
                total += e - s;
            }
        }
    }
    total as i64
}

/// Progress as qBittorrent shows it: wanted bytes had / wanted bytes.
pub fn wanted_progress(m: &TorrentMeta, priorities: &[i32], have: &[bool]) -> f64 {
    let wanted: u64 = m.files.iter().zip(priorities).filter(|(_, p)| **p != 0).map(|(f, _)| f.size).sum();
    if wanted == 0 {
        return 0.0;
    }
    completed_wanted_bytes(m, priorities, have) as f64 / wanted as f64
}

/// The engine's fast-resume bitfield (`<engine hash>.bitv`): one bit per
/// piece, most significant bit first.
pub fn engine_bitfield(have: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; have.len().div_ceil(8)];
    for (i, h) in have.iter().enumerate() {
        if *h {
            out[i / 8] |= 0x80 >> (i % 8);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bittorrent::bencode::encode;
    use std::collections::BTreeMap;

    pub(crate) fn dict(entries: Vec<(&str, Value)>) -> Value {
        Value::Dict(entries.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect::<BTreeMap<_, _>>())
    }
    pub(crate) fn b(s: &str) -> Value {
        Value::Bytes(s.as_bytes().to_vec())
    }
    pub(crate) fn enc(v: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        encode(v, &mut out);
        out
    }

    /// A two-file torrent: "dir/a.mkv" (3 pieces worth) and "dir/b.nfo".
    pub(crate) fn sample_torrent(piece_len: usize, a: &[u8], nfo: &[u8]) -> Vec<u8> {
        let mut all = a.to_vec();
        all.extend_from_slice(nfo);
        let mut hashes = Vec::new();
        for chunk in all.chunks(piece_len) {
            let mut h = sha1w::Sha1::new();
            use sha1w::ISha1;
            h.update(chunk);
            hashes.extend_from_slice(&h.finish());
        }
        let info = dict(vec![
            ("name", b("dir")),
            ("piece length", Value::Int(piece_len as i64)),
            ("pieces", Value::Bytes(hashes)),
            (
                "files",
                Value::List(vec![
                    dict(vec![("length", Value::Int(a.len() as i64)), ("path", Value::List(vec![b("a.mkv")]))]),
                    dict(vec![("length", Value::Int(nfo.len() as i64)), ("path", Value::List(vec![b("b.nfo")]))]),
                ]),
            ),
        ]);
        enc(&dict(vec![("comment", b("hello")), ("info", info)]))
    }

    pub(crate) fn sample_resume(save_path: &str, pieces: &[u8]) -> Vec<u8> {
        enc(&dict(vec![
            ("file-format", b("libtorrent resume file")),
            ("info-hash", Value::Bytes(vec![0; 20])),
            ("pieces", Value::Bytes(pieces.to_vec())),
            ("file_priority", Value::List(vec![Value::Int(1), Value::Int(0)])),
            ("mapped_files", Value::List(vec![b("a.mkv"), b("b.nfo")])),
            (
                "trackers",
                Value::List(vec![
                    Value::List(vec![b("http://t1/announce?passkey=x"), b("udp://t2:80")]),
                    Value::List(vec![b("http://t3/a")]),
                ]),
            ),
            ("url-list", Value::List(vec![b("http://seed/x"), b("gopher://nope")])),
            ("httpseeds", Value::List(vec![b("http://hs/")])),
            ("save_path", b(save_path)),
            ("paused", Value::Int(1)),
            ("auto_managed", Value::Int(0)),
            ("sequential_download", Value::Int(1)),
            ("added_time", Value::Int(1_700_000_000)),
            ("completed_time", Value::Int(1_700_000_100)),
            ("last_seen_complete", Value::Int(0)),
            ("active_time", Value::Int(500)),
            ("seeding_time", Value::Int(400)),
            ("last_upload", Value::Int(0)),
            ("last_download", Value::Int(1_700_000_050)),
            ("total_uploaded", Value::Int(1234)),
            ("total_downloaded", Value::Int(5678)),
            ("upload_rate_limit", Value::Int(-1)),
            ("download_rate_limit", Value::Int(2048)),
            ("unfinished", Value::List(vec![dict(vec![("piece", Value::Int(1))])])),
            ("qBt-category", b("Movies/HD")),
            ("qBt-tags", Value::List(vec![b("Movie"), b("\u{77e2}\u{6ca2}"), b("bad,tag")])),
            ("qBt-name", b("")),
            ("qBt-comment", b("hello")),
            ("qBt-savePath", b("")),
            ("qBt-ratioLimit", Value::Int(1500)),
            ("qBt-seedingTimeLimit", Value::Int(-2)),
            ("qBt-shareLimitAction", b("Default")),
            ("qBt-contentLayout", b("NoSubfolder")),
            ("qBt-stopCondition", b("None")),
            ("qBt-seedStatus", Value::Int(1)),
            ("qBt-firstLastPiecePriority", Value::Int(1)),
        ]))
    }

    #[test]
    fn decodes_libtorrent_and_qbt_keys() {
        let data = sample_resume("/Users/x/Movies", &[1, 1, 0, 3]);
        let r = LtResume::parse(&data, Limits::default()).unwrap();
        assert_eq!(r.pieces.as_deref(), Some(&[1u8, 1, 0, 3][..]));
        assert_eq!(r.file_priority, vec![1, 0]);
        assert_eq!(r.mapped_files, vec!["a.mkv", "b.nfo"]);
        assert_eq!(r.trackers.len(), 2);
        assert_eq!(r.url_seeds.len(), 2);
        assert_eq!(r.http_seeds, vec!["http://hs/"]);
        assert!(r.stopped());
        assert!(!r.forced());
        assert_eq!(r.ratio_limit, 1.5);
        assert_eq!(r.content_layout, "NoSubfolder");
        assert_eq!(r.unfinished_pieces, 1);
        assert_eq!(r.have_pieces(4).unwrap(), vec![true, true, false, true]);
        assert!(r.have_pieces(5).is_err());

        // String ratio limit (qBittorrent 5), the old root folder flag,
        // forced mode.
        let data = enc(&dict(vec![
            ("save_path", b("/x")),
            ("qBt-ratioLimit", b("2.25")),
            ("qBt-hasRootFolder", Value::Int(0)),
            ("paused", Value::Int(0)),
            ("auto_managed", Value::Int(0)),
            ("seed_mode", Value::Int(1)),
            ("pieces", Value::Bytes(vec![1])),
        ]));
        let r = LtResume::parse(&data, Limits::default()).unwrap();
        assert_eq!(r.ratio_limit, 2.25);
        assert_eq!(r.content_layout, "NoSubfolder");
        assert!(r.forced() && !r.stopped());
        assert!(r.have_pieces(1).is_err(), "seed mode data is unverified");

        assert!(LtResume::parse(b"d4:infoi1ee", Limits::default()).is_err(), "no save_path");
        assert!(LtResume::parse(b"li1ee", Limits::default()).is_err());
        assert!(LtResume::parse(b"d3:abc", Limits::default()).is_err());
    }

    #[test]
    fn converts_to_our_record() {
        let tor = sample_torrent(16, &[7u8; 40], b"nfo!");
        let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
        assert_eq!(meta.pieces, 3);
        let data = sample_resume("/Users/x/Movies", &[1, 1, 1]);
        let r = LtResume::parse(&data, Limits::default()).unwrap();
        let have = r.have_pieces(meta.pieces).unwrap();
        let (fr, notes) = to_fast_resume(&r, &meta.id(), Some(&meta), Some(&have), 3);
        assert_eq!(fr.info_hash, meta.id());
        assert!(fr.auto_tmm && fr.save_path.is_empty());
        assert_eq!(fr.location, "/Users/x/Movies");
        assert_eq!(fr.category, "Movies/HD");
        assert_eq!(fr.tags, vec!["Movie", "\u{77e2}\u{6ca2}"]);
        assert!(notes.iter().any(|n| n.contains("bad,tag")));
        assert_eq!(fr.file_paths, vec!["a.mkv", "b.nfo"]);
        assert_eq!(fr.file_priorities, vec![1, 0]);
        assert_eq!(fr.comment, None, "same as the metadata comment");
        assert_eq!(fr.trackers.len(), 3);
        assert_eq!((fr.trackers[2].url.as_str(), fr.trackers[2].tier), ("http://t3/a", 1));
        assert_eq!(fr.url_seeds, vec!["http://seed/x"]);
        assert_eq!(fr.trusted_urls.len(), 4);
        assert!(fr.stopped);
        assert_eq!(fr.operating_mode, "AutoManaged");
        assert_eq!(fr.ratio_limit, 1.5);
        assert_eq!((fr.added_on, fr.completed_on, fr.seen_complete), (1_700_000_000, 1_700_000_100, -1));
        assert_eq!((fr.last_upload, fr.last_download), (-1, 1_700_000_050));
        assert_eq!((fr.all_time_upload, fr.all_time_download, fr.seeding_time), (1234, 5678, 400));
        assert_eq!((fr.download_limit, fr.upload_limit), (2048, 0));
        assert!(fr.sequential && fr.first_last_piece_priority && fr.seed_status);
        assert_eq!(fr.queue_position, 3);
        assert_eq!(fr.completed_bytes, 40);
        assert_eq!(fr.content_layout, "NoSubfolder");
        assert_eq!(wanted_progress(&meta, &fr.file_priorities, &[true, false, true]), 24.0 / 40.0);
    }

    #[test]
    fn mapped_paths_and_priorities() {
        let tor = sample_torrent(16, &[7u8; 40], b"nfo!");
        let meta = metainfo::parse_torrent(&tor, Limits::default()).unwrap().meta;
        let mut r = LtResume::parse(&sample_resume("/s", &[1, 1, 1]), Limits::default()).unwrap();
        // Incomplete-file suffix stripped, unmapped files keep their path,
        // libtorrent priorities folded to qBittorrent's levels.
        r.mapped_files = vec!["Movie/a.mkv.!qB".into()];
        r.file_priority = vec![4, 6];
        r.qbt_save_path = "/manual".into();
        r.qbt_download_path = "/tmp/dl".into();
        let (fr, notes) = to_fast_resume(&r, &meta.id(), Some(&meta), None, -1);
        assert_eq!(fr.file_paths, vec!["Movie/a.mkv", "dir/b.nfo"]);
        assert_eq!(fr.file_priorities, vec![1, 6]);
        assert!(!fr.auto_tmm);
        assert_eq!((fr.save_path.as_str(), fr.download_path.as_str()), ("/manual", "/tmp/dl"));
        assert!(notes.iter().any(|n| n.contains("renamed")));
        // Paths escaping the save folder are refused.
        r.mapped_files = vec!["../evil".into()];
        let (fr, notes) = to_fast_resume(&r, &meta.id(), Some(&meta), None, -1);
        assert_eq!(fr.file_paths, vec!["a.mkv", "b.nfo"], "the torrent's content layout (NoSubfolder) applied");
        assert!(notes.iter().any(|n| n.contains("invalid")));
    }

    #[test]
    fn bitfield_and_queue() {
        assert_eq!(engine_bitfield(&[true, false, true, true, false, false, false, false, true]), vec![0b1011_0000, 0b1000_0000]);
        assert_eq!(engine_bitfield(&[]), Vec::<u8>::new());
        let q = parse_queue(b"B25B2A1EEEB73D403349B33C22DD2C93C72DECC7\nnot a hash\n05b49e5545146c975a63351baf8a0f7051da5ce5\r\n");
        assert_eq!(q, vec!["b25b2a1eeeb73d403349b33c22dd2c93c72decc7", "05b49e5545146c975a63351baf8a0f7051da5ce5"]);
    }
}
