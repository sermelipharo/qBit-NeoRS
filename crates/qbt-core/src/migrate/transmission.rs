//! Migration of the Transmission app for macOS (3.x and 4.x) into a qBit
//! NeoRS profile (`qbit-neors-nox migrate-from-transmission`,
//! docs/MIGRATION.md).
//!
//! Everything of Transmission is only read: its folder
//! `~/Library/Application Support/Transmission` (`Torrents/`, `Resume/`,
//! `Transfers.plist`, `queue.json`, `stats.json`) and its settings, the
//! `org.m0k.transmission` defaults domain (an exported property list, or
//! `defaults export` which only reads). Content files are only looked at
//! (existence, size, modification time), never opened for writing, moved or
//! renamed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::gui::PlistValue as GuiValue;
use super::plist::{self, Plist};
use super::{Report, Sandbox, TorrentSummary, Writer, MAX_CONFIG};
use crate::bittorrent::bencode::{self, Limits, Value};
use crate::bittorrent::categories::{self, CategoryMap};
use crate::bittorrent::metainfo::{self, TorrentMeta};
use crate::bittorrent::resume::{self, BackupDir, FastResume, SessionStateFile, StatsFile};
use crate::bittorrent::paths;
use crate::model::{FilePriority, TrackerTierEntry};
use crate::prefs::Preferences;
use crate::prefs::{BtProtocol, EncryptionMode, SchedulerDays};

/// The defaults domain of the Transmission app.
pub const DEFAULTS_DOMAIN: &str = "org.m0k.transmission";
/// Its bundle identifier (the same string).
pub const BUNDLE_ID: &str = "org.m0k.transmission";
/// Transmission's block size: its resume data has one bit per block.
pub const BLOCK_SIZE: u64 = 16 * 1024;
/// Transmission's speed unit on macOS (kB/s = 1000 bytes/s).
const SPEED_K: i64 = 1000;

#[derive(Clone, Debug)]
pub struct Options {
    /// `~/Library/Application Support/Transmission`.
    pub source_dir: PathBuf,
    /// An exported copy of the defaults domain (`defaults export
    /// org.m0k.transmission file.plist`). None: read the domain itself with
    /// `defaults export` when `read_system_defaults` is set.
    pub defaults_plist: Option<PathBuf>,
    pub read_system_defaults: bool,
    pub profile: PathBuf,
    pub dry_run: bool,
    pub stop_all: bool,
    pub sandbox: Option<Sandbox>,
    pub force: bool,
    pub merge: bool,
    /// Write the GUI settings to `migration/` only (the app applies them
    /// itself through its defaults store).
    pub gui_plist_in_migration: bool,
}

impl Options {
    /// Transmission's default locations on macOS for `home`.
    /// The settings come from the defaults domain for the user's own home,
    /// from `<home>/Library/Preferences/org.m0k.transmission.plist` for
    /// any other folder (tests).
    pub fn macos_defaults(home: &Path, profile: PathBuf) -> Self {
        let own_home = std::env::var_os("HOME").is_some_and(|h| Path::new(&h) == home);
        Self {
            source_dir: home.join("Library/Application Support/Transmission"),
            defaults_plist: (!own_home).then(|| home.join(format!("Library/Preferences/{DEFAULTS_DOMAIN}.plist"))),
            read_system_defaults: own_home,
            profile,
            dry_run: false,
            stop_all: false,
            sandbox: None,
            force: false,
            merge: false,
            gui_plist_in_migration: false,
        }
    }
}

// ---------------------------------------------------------------- resume data

/// Transmission's block bitfield (`progress.blocks`).
#[derive(Clone, Debug, PartialEq)]
pub enum Blocks {
    All,
    None,
    /// One bit per 16 KiB block, most significant bit first.
    Raw(Vec<u8>),
    /// No block information: the torrent needs a check.
    Missing,
}

/// A decoded `.resume` file. Times are Unix seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct TrResume {
    pub destination: String,
    pub incomplete_dir: String,
    pub downloaded: i64,
    pub uploaded: i64,
    pub added_date: i64,
    pub done_date: i64,
    pub activity_date: i64,
    pub seeding_seconds: i64,
    pub downloading_seconds: i64,
    pub paused: bool,
    pub bandwidth_priority: i64,
    pub sequential: bool,
    pub labels: Vec<String>,
    /// Bandwidth group (4.x), unrelated to the app's groups.
    pub bandwidth_group: String,
    /// Per file: "do not download".
    pub dnd: Vec<bool>,
    /// Per file: -1 low, 0 normal, 1 high.
    pub priority: Vec<i64>,
    /// Own limit in bytes/s when `use-speed-limit` is on.
    pub upload_limit: Option<i64>,
    pub download_limit: Option<i64>,
    pub honors_session_limits: bool,
    /// 0 global, 1 own, 2 unlimited.
    pub ratio_mode: i64,
    pub ratio_limit: f64,
    pub idle_mode: i64,
    /// Minutes.
    pub idle_limit: i64,
    /// Renamed torrent ("" = the metadata's name).
    pub name: String,
    /// File paths after renames (empty entries = not renamed).
    pub files: Vec<String>,
    pub blocks: Blocks,
    /// 4.x: per file, its modification time when its pieces were checked.
    pub mtimes: Option<Vec<i64>>,
    /// 3.x: per file, the oldest piece check time (0 = unchecked).
    pub time_checked: Option<Vec<i64>>,
    pub peer_count: usize,
}

fn get<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| v.get(k))
}

fn int(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(Value::as_int).unwrap_or(default)
}

fn text(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or_default()
}

impl TrResume {
    pub fn parse(data: &[u8], limits: Limits) -> Result<Self, String> {
        let root = bencode::decode(data, limits).map_err(|e| format!("Cannot parse resume data: {e}"))?;
        if root.as_dict().is_none() {
            return Err("Cannot parse resume data: invalid format".into());
        }
        let r = &root;
        let speed = |key: &[&str]| -> (Option<i64>, bool) {
            match get(r, key) {
                Some(d) => {
                    let bps = get(d, &["speed-Bps", "speed_bps"]).and_then(Value::as_int).or_else(|| d.get("speed").and_then(Value::as_int).map(|k| k * 1024));
                    let own = int(get(d, &["use-speed-limit", "use_speed_limit"]), 0) != 0;
                    let global = int(get(d, &["use-global-speed-limit", "use_global_speed_limit"]), 1) != 0;
                    (if own { bps.filter(|b| *b > 0) } else { None }, global)
                }
                None => (None, true),
            }
        };
        let (upload_limit, up_global) = speed(&["speed-limit-up", "speed_limit_up"]);
        let (download_limit, down_global) = speed(&["speed-limit-down", "speed_limit_down"]);
        let ratio = get(r, &["ratio-limit", "ratio_limit"]);
        let ratio_limit = match ratio.and_then(|d| get(d, &["ratio-limit", "ratio_limit"])) {
            Some(Value::Bytes(b)) => String::from_utf8_lossy(b).trim().parse().unwrap_or(2.0),
            Some(Value::Int(i)) => *i as f64,
            _ => 2.0,
        };
        let idle = get(r, &["idle-limit", "idle_limit"]);
        let progress = get(r, &["progress"]);
        let blocks = match progress.and_then(|p| p.get("blocks")).or_else(|| progress.and_then(|p| p.get("bitfield"))) {
            Some(Value::Bytes(b)) if b == b"all" => Blocks::All,
            Some(Value::Bytes(b)) if b == b"none" || b.is_empty() => Blocks::None,
            Some(Value::Bytes(b)) => Blocks::Raw(b.clone()),
            _ if progress.and_then(|p| p.get("have")).and_then(Value::as_bytes) == Some(b"all") => Blocks::All,
            _ => Blocks::Missing,
        };
        let int_list = |v: Option<&Value>| -> Option<Vec<i64>> { v.and_then(Value::as_list).map(|l| l.iter().map(|x| x.as_int().unwrap_or(0)).collect()) };
        let time_checked = progress.and_then(|p| get(p, &["time-checked", "time_checked"])).and_then(Value::as_list).map(|l| {
            l.iter()
                .map(|x| match x {
                    Value::Int(i) => *i,
                    // [base, offset per piece...]: 0 offsets are unchecked pieces.
                    Value::List(parts) => {
                        let base = parts.first().and_then(Value::as_int).unwrap_or(0);
                        let offs: Vec<i64> = parts.iter().skip(1).map(|p| p.as_int().unwrap_or(0)).collect();
                        if base == 0 || offs.contains(&0) { 0 } else { base + offs.iter().copied().min().unwrap_or(0) }
                    }
                    _ => 0,
                })
                .collect()
        });
        let peers = |k: &str| match r.get(k) {
            Some(Value::List(l)) => l.len(),
            Some(Value::Bytes(b)) => usize::from(!b.is_empty()),
            _ => 0,
        };
        let out = TrResume {
            destination: text(get(r, &["destination"])),
            incomplete_dir: text(get(r, &["incomplete-dir", "incomplete_dir"])),
            downloaded: int(get(r, &["downloaded"]), 0).max(0),
            uploaded: int(get(r, &["uploaded"]), 0).max(0),
            added_date: int(get(r, &["added-date", "added_date"]), 0),
            done_date: int(get(r, &["done-date", "done_date"]), 0),
            activity_date: int(get(r, &["activity-date", "activity_date"]), 0),
            seeding_seconds: int(get(r, &["seeding-time-seconds", "seeding_time_seconds"]), 0).max(0),
            downloading_seconds: int(get(r, &["downloading-time-seconds", "downloading_time_seconds"]), 0).max(0),
            paused: int(get(r, &["paused"]), 0) != 0,
            bandwidth_priority: int(get(r, &["bandwidth-priority", "bandwidth_priority"]), 0),
            sequential: int(get(r, &["sequential_download", "sequential-download"]), 0) != 0,
            labels: get(r, &["labels"]).and_then(Value::as_list).map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default(),
            bandwidth_group: text(get(r, &["group"])),
            dnd: int_list(get(r, &["dnd"])).map(|l| l.into_iter().map(|x| x != 0).collect()).unwrap_or_default(),
            priority: match get(r, &["priority"]) {
                Some(Value::List(_)) => int_list(get(r, &["priority"])).unwrap_or_default(),
                _ => Vec::new(),
            },
            upload_limit,
            download_limit,
            honors_session_limits: up_global && down_global,
            ratio_mode: int(ratio.and_then(|d| get(d, &["ratio-mode", "ratio_mode"])), 0),
            ratio_limit,
            idle_mode: int(idle.and_then(|d| get(d, &["idle-mode", "idle_mode"])), 0),
            idle_limit: int(idle.and_then(|d| get(d, &["idle-limit", "idle_limit"])), 30),
            name: text(get(r, &["name"])),
            files: get(r, &["files"]).and_then(Value::as_list).map(|l| l.iter().map(|x| x.as_str().unwrap_or_default()).collect()).unwrap_or_default(),
            blocks,
            mtimes: int_list(progress.and_then(|p| p.get("mtimes"))),
            time_checked,
            peer_count: peers("peers2") + peers("peers2-6"),
        };
        if out.destination.is_empty() {
            return Err("Corrupted resume data: no destination".into());
        }
        Ok(out)
    }

    /// The pieces Transmission holds: those whose blocks are all set.
    pub fn have_pieces(&self, m: &TorrentMeta) -> Result<Vec<bool>, String> {
        let n = m.pieces as usize;
        match &self.blocks {
            Blocks::All => Ok(vec![true; n]),
            Blocks::None => Ok(vec![false; n]),
            Blocks::Missing => Err("no block bitfield in the resume data".into()),
            Blocks::Raw(bits) => {
                let block_count = m.total_size.div_ceil(BLOCK_SIZE);
                if (bits.len() as u64) > block_count.div_ceil(8) {
                    return Err(format!("block bitfield has {} bytes, the torrent has {block_count} blocks", bits.len()));
                }
                let has = |b: u64| bits.get((b / 8) as usize).is_some_and(|x| x & (0x80 >> (b % 8)) != 0);
                let plen = u64::from(m.piece_length);
                Ok((0..n as u64)
                    .map(|p| {
                        let start = p * plen;
                        let end = ((p + 1) * plen).min(m.total_size);
                        end > start && (start / BLOCK_SIZE..=(end - 1) / BLOCK_SIZE).all(has)
                    })
                    .collect())
            }
        }
    }
}

/// Index into a Transmission per-file list for one of our files: lists
/// count padding files when their length says so.
fn tr_index(list_len: usize, m: &TorrentMeta, i: usize) -> usize {
    if list_len == m.engine_file_count as usize && list_len != m.files.len() { m.files[i].engine_index as usize } else { i }
}

// ---------------------------------------------------------------- app data

/// One entry of `Transfers.plist`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transfer {
    pub active: bool,
    pub wait_to_start: bool,
    pub group: i64,
    pub remove_when_finished_seeding: bool,
}

/// `Transfers.plist`: per info hash (lowercase hex), plus the hashes in order.
pub fn parse_transfers(data: &[u8]) -> Result<(BTreeMap<String, Transfer>, Vec<String>), String> {
    let root = plist::parse(data)?;
    let list = root.as_array().ok_or("Transfers.plist is not a list")?;
    let mut map = BTreeMap::new();
    let mut order = Vec::new();
    for item in list {
        let Some(hash) = item.get("TorrentHash").and_then(Plist::as_str).map(str::to_ascii_lowercase) else { continue };
        let b = |k: &str| item.get(k).and_then(Plist::as_bool).unwrap_or(false);
        let t = Transfer {
            active: b("Active"),
            wait_to_start: b("WaitToStart"),
            group: item.get("GroupValue").and_then(Plist::as_i64).unwrap_or(-1),
            remove_when_finished_seeding: b("RemoveWhenFinishSeeding"),
        };
        order.push(hash.clone());
        map.insert(hash, t);
    }
    Ok((map, order))
}

/// One of the app's groups (`GroupDicts`).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub index: i64,
    pub name: String,
    pub custom_location: Option<String>,
    pub has_rules: bool,
}

/// The groups of the defaults (`GroupDicts`, an archived array of
/// dictionaries); Transmission's seven color groups when there are none.
pub fn parse_groups(defaults: &Plist) -> Result<Vec<Group>, String> {
    let Some(data) = defaults.get("GroupDicts").and_then(Plist::as_data) else {
        return Ok(["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Gray"]
            .iter()
            .enumerate()
            .map(|(i, n)| Group { index: i as i64, name: (*n).into(), custom_location: None, has_rules: false })
            .collect());
    };
    let list = plist::unarchive(data)?;
    let list = list.as_array().ok_or("GroupDicts is not a list")?;
    Ok(list
        .iter()
        .filter_map(|g| {
            let index = g.get("Index").and_then(Plist::as_i64)?;
            let uses = g.get("UsesCustomDownloadLocation").and_then(Plist::as_bool).unwrap_or(false);
            Some(Group {
                index,
                name: g.get("Name").and_then(Plist::as_str).unwrap_or_default().trim().to_owned(),
                custom_location: g.get("CustomDownloadLocation").and_then(Plist::as_str).filter(|p| uses && !p.is_empty()).map(expand_tilde),
                has_rules: g.get("UsesAutoGroupRules").and_then(Plist::as_bool).unwrap_or(false),
            })
        })
        .collect())
}

/// `queue.json` (4.x): torrent file names in queue order, as hashes.
pub fn parse_queue(data: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Vec<String>>(data)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|n| n.split('.').next().map(str::to_ascii_lowercase))
        .filter(|h| h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        .collect()
}

/// `stats.json`: (downloaded, uploaded) bytes.
pub fn parse_stats(data: &[u8]) -> Option<(i64, i64)> {
    let v: serde_json::Value = serde_json::from_slice(data).ok()?;
    let g = |k: &str| v.get(k).and_then(serde_json::Value::as_i64).unwrap_or(0).max(0);
    Some((g("downloaded-bytes"), g("uploaded-bytes")))
}

fn expand_tilde(p: &str) -> String {
    match (p.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home.to_string_lossy()),
        _ => p.to_owned(),
    }
}

/// The defaults domain, read only.
pub(crate) fn read_defaults(opts: &Options, rep: &mut Report) -> Plist {
    let data = if let Some(file) = &opts.defaults_plist {
        match resume::read_limited(file, MAX_CONFIG) {
            Ok(d) => {
                rep.sources.push(format!("settings: {}", file.display()));
                Some(d)
            }
            Err(e) => {
                rep.warnings.push(format!("{}: {e}; Transmission's default settings assumed", file.display()));
                None
            }
        }
    } else if opts.read_system_defaults {
        match std::process::Command::new("/usr/bin/defaults").args(["export", DEFAULTS_DOMAIN, "-"]).output() {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                rep.sources.push(format!("settings: defaults domain {DEFAULTS_DOMAIN}"));
                Some(out.stdout)
            }
            _ => {
                rep.warnings.push(format!("the defaults domain {DEFAULTS_DOMAIN} could not be read; Transmission's default settings assumed"));
                None
            }
        }
    } else {
        None
    };
    match data.map(|d| plist::parse(&d)) {
        Some(Ok(v @ Plist::Dict(_))) => v,
        Some(Ok(_)) | Some(Err(_)) => {
            rep.warnings.push("Transmission's settings are not a readable property list; its default settings assumed".into());
            Plist::Dict(BTreeMap::new())
        }
        None => Plist::Dict(BTreeMap::new()),
    }
}

/// A running Transmission app, if any.
pub fn transmission_running() -> Option<String> {
    let out = std::process::Command::new("/bin/ps").args(["-axww", "-o", "pid=,args="]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|line| {
        let (pid, args) = line.trim().split_once(' ')?;
        args.contains("/Contents/MacOS/Transmission").then(|| format!("process {pid}"))
    })
}

// ---------------------------------------------------------------- settings

/// What the defaults map to, plus the GUI settings and the session state.
pub struct MappedSettings {
    pub prefs: Preferences,
    pub gui: Vec<(String, GuiValue)>,
    pub alt_speed: bool,
    /// Remote access (RPC) was on.
    pub remote_access: bool,
    pub notes: Vec<String>,
    /// reason -> settings left out
    pub unmapped: BTreeMap<String, Vec<String>>,
}

fn local_hour_min(unix: f64) -> (i64, i64) {
    use chrono::{Local, TimeZone, Timelike};
    match Local.timestamp_opt(unix as i64, 0).single() {
        Some(t) => (i64::from(t.hour()), i64::from(t.minute())),
        None => (0, 0),
    }
}

/// Maps the settings of the defaults domain that have an equivalent.
pub fn map_settings(d: &Plist) -> MappedSettings {
    let mut p = Preferences::default();
    let mut notes = Vec::new();
    let mut unmapped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut skip = |reason: &str, what: String| unmapped.entry(reason.to_owned()).or_default().push(what);
    let b = |k: &str, def: bool| d.get(k).and_then(Plist::as_bool).unwrap_or(def);
    let i = |k: &str, def: i64| d.get(k).and_then(Plist::as_i64).unwrap_or(def);
    let s = |k: &str| d.get(k).and_then(Plist::as_str).map(str::to_owned).unwrap_or_default();

    // Folders.
    let download = s("DownloadFolder");
    p.downloads.save_path = expand_tilde(if download.is_empty() { "~/Downloads" } else { &download });
    notes.push(format!("default save path {}", p.downloads.save_path));
    if !b("DownloadLocationConstant", false) && d.get("DownloadLocationConstant").is_some() {
        skip("no equivalent", "\"same folder as the .torrent file\" download location (the default folder is used)".into());
    }
    if b("UseIncompleteDownloadFolder", false) {
        let inc = s("IncompleteDownloadFolder");
        p.downloads.temp_path_enabled = true;
        p.downloads.temp_path = expand_tilde(if inc.is_empty() { "~/Downloads" } else { &inc });
        notes.push(format!("incomplete torrents in {}", p.downloads.temp_path));
    }
    if b("RenamePartialFiles", true) {
        skip(
            "different",
            "\".part\" suffix of incomplete files (files that still have it keep that name; the \".!qB\" option stays off)".into(),
        );
    }
    p.downloads.add_stopped_enabled = !b("AutoStartDownload", true);
    if b("DeleteOriginalTorrent", false) {
        p.downloads.auto_delete_mode = crate::prefs::AutoDeleteMode::IfAdded;
    }
    if b("AutoImport", false) {
        let dir = s("AutoImportDirectory");
        if !dir.is_empty() {
            p.downloads.watched_folders.push(crate::prefs::WatchedFolder { path: expand_tilde(&dir), recursive: false, params: Default::default() });
            notes.push("watched folder".into());
        }
    }
    if b("DoneScriptEnabled", false) {
        skip(
            "different",
            "script run when a download completes (Transmission passes TR_* environment variables; set it up again under Downloads > Run external program)".into(),
        );
    }

    // Speed.
    if b("CheckDownload", false) {
        p.speed.dl_limit = i("DownloadLimit", 100).max(0) * SPEED_K;
    }
    if b("CheckUpload", false) {
        p.speed.up_limit = i("UploadLimit", 50).max(0) * SPEED_K;
    }
    p.speed.alt_dl_limit = i("SpeedLimitDownloadLimit", 10).max(0) * SPEED_K;
    p.speed.alt_up_limit = i("SpeedLimitUploadLimit", 10).max(0) * SPEED_K;
    if b("SpeedLimitAuto", false) {
        p.speed.scheduler_enabled = true;
        let on = d.get("SpeedLimitAutoOnDate").and_then(Plist::as_f64).map(local_hour_min).unwrap_or((3, 0));
        let off = d.get("SpeedLimitAutoOffDate").and_then(Plist::as_f64).map(local_hour_min).unwrap_or((13, 0));
        (p.speed.schedule_from_hour, p.speed.schedule_from_min) = on;
        (p.speed.schedule_to_hour, p.speed.schedule_to_min) = off;
        let days = i("SpeedLimitAutoDay", 127);
        p.speed.scheduler_days = match days {
            127 => SchedulerDays::EveryDay,
            62 => SchedulerDays::Weekday,
            65 => SchedulerDays::Weekend,
            1 => SchedulerDays::Sunday,
            2 => SchedulerDays::Monday,
            4 => SchedulerDays::Tuesday,
            8 => SchedulerDays::Wednesday,
            16 => SchedulerDays::Thursday,
            32 => SchedulerDays::Friday,
            64 => SchedulerDays::Saturday,
            _ => {
                skip("no equivalent", format!("alternative speed schedule on the day set {days:#09b} (every day is used)"));
                SchedulerDays::EveryDay
            }
        };
        notes.push("alternative speed schedule".into());
    }
    let alt_speed = !b("SpeedLimitAuto", false) && b("SpeedLimit", false);

    // Connection.
    if let Some(port) = d.get("BindPort").and_then(Plist::as_i64).filter(|p| (1..=65535).contains(p)) {
        p.connection.listen_port = port;
    }
    if b("RandomPort", false) {
        skip("no equivalent", "random peer port at each start (the last port is kept)".into());
    }
    p.connection.upnp = b("NatTraversal", true);
    p.connection.max_connec = i("PeersTotal", 200).max(1);
    p.connection.max_connec_per_torrent = i("PeersTorrent", 60).max(1);
    p.bittorrent.encryption = match (b("EncryptionPrefer", true), b("EncryptionRequire", false)) {
        (_, true) => EncryptionMode::Require,
        (true, false) => EncryptionMode::Allow,
        (false, false) => {
            notes.push("encryption: Transmission preferred unencrypted connections; qBit NeoRS allows both".into());
            EncryptionMode::Allow
        }
    };
    p.bittorrent.dht = b("DHTGlobal", true);
    p.bittorrent.pex = b("PEXGlobal", true);
    p.bittorrent.lsd = b("LocalPeerDiscoveryGlobal", false);
    if !b("UTPGlobal", true) {
        p.connection.bittorrent_protocol = BtProtocol::Tcp;
    }
    if b("BlocklistNew", false) {
        skip(
            "not carried over",
            "blocklist downloaded from a URL (download the list and choose it under Connection > IP Filtering)".into(),
        );
    }
    for k in ["BindAddressIPv4", "BindAddressIPv6"] {
        if d.get(k).is_some() {
            skip("no equivalent", format!("{k} (choose the network interface or address under Advanced)"));
        }
    }

    // Queue and seeding limits.
    p.bittorrent.queueing_enabled = b("Queue", false) || b("QueueSeed", false);
    if p.bittorrent.queueing_enabled {
        let dl = if b("Queue", false) { i("QueueDownloadNumber", 3).max(1) } else { -1 };
        let ul = if b("QueueSeed", false) { i("QueueSeedNumber", 3).max(1) } else { -1 };
        p.bittorrent.max_active_downloads = dl;
        p.bittorrent.max_active_uploads = ul;
        p.bittorrent.max_active_torrents = if dl < 0 || ul < 0 { -1 } else { dl + ul };
    }
    if b("CheckStalled", true) {
        p.bittorrent.dont_count_slow_torrents = true;
        p.bittorrent.slow_torrent_inactive_timer = i("StalledMinutes", 30).max(1) * 60;
    }
    p.bittorrent.max_ratio = if b("RatioCheck", false) { d.get("RatioLimit").and_then(Plist::as_f64).unwrap_or(2.0).max(0.0) } else { -1.0 };
    p.bittorrent.max_inactive_seeding_time = if b("IdleLimitCheck", false) { i("IdleLimitMinutes", 30).max(1) } else { -1 };
    p.bittorrent.max_ratio_act = crate::model::ShareLimitAction::Stop;
    p.behavior.confirm_torrent_deletion = b("CheckRemove", true);

    // Remote access: never the password (it is in Transmission's keychain item).
    if b("RPC", false) {
        skip(
            "not carried over",
            format!(
                "Transmission remote access (RPC, port {}{}): its password is not copied; qBit NeoRS offers the qBittorrent Web UI and WebAPI instead (Settings > Web UI)",
                i("RPCPort", 9091),
                if b("RPCAuthorize", false) { ", with a password" } else { "" }
            ),
        );
    }
    if b("SUEnableAutomaticChecks", false) || d.get("AutoUpdateBeta").is_some() {
        skip("app specific", "update checks of the Transmission app".into());
    }
    let ui_state = ["Sort", "SortReverse", "SortByGroup", "Filter", "FilterGroup", "FilterSearchType", "SmallView", "PiecesBar", "InspectorSelected"];
    let found: Vec<String> = ui_state.iter().filter(|k| d.get(k).is_some()).map(|k| (*k).to_owned()).collect();
    if !found.is_empty() {
        skip("app specific", format!("window state ({})", found.join(", ")));
    }

    // GUI settings of the app.
    let mut gui = Vec::new();
    if let Some(v) = d.get("CheckQuit").and_then(Plist::as_bool) {
        gui.push(("Preferences/General/ExitConfirm".to_owned(), GuiValue::Bool(v)));
    }
    if let Some(v) = d.get("DownloadAsk").and_then(Plist::as_bool) {
        gui.push(("AddNewTorrentDialog/Enabled".to_owned(), GuiValue::Bool(v)));
    }
    if let Some(v) = d.get("SleepPrevent").and_then(Plist::as_bool) {
        gui.push(("Preferences/General/PreventFromSuspendWhenDownloading".to_owned(), GuiValue::Bool(v)));
        gui.push(("Preferences/General/PreventFromSuspendWhenSeeding".to_owned(), GuiValue::Bool(v)));
    }
    MappedSettings { prefs: p, gui, alt_speed, remote_access: b("RPC", false), notes, unmapped }
}

// ---------------------------------------------------------------- torrents

/// A torrent found in `Torrents/`.
struct Source {
    /// Transmission's hash (v1, lowercase hex): the name of its files.
    tr_hash: String,
    torrent: Option<(Vec<u8>, metainfo::ParsedTorrent)>,
    magnet: Option<metainfo::MagnetLink>,
    resume: Option<PathBuf>,
}

fn find_legacy(dir: &Path, hash: &str, ext: &str) -> Option<PathBuf> {
    let suffix = format!(".{}.{ext}", &hash[..16]);
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_ascii_lowercase().ends_with(&suffix)))
}

fn scan_sources(dir: &Path, limits: Limits, rep: &mut Report) -> Vec<Source> {
    let torrents = dir.join("Torrents");
    let resumes = dir.join("Resume");
    let mut out: BTreeMap<String, Source> = BTreeMap::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&torrents).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    entries.sort();
    for path in entries {
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if name.starts_with('.') || !path.is_file() {
            continue;
        }
        if name.ends_with(".torrent") {
            let Ok(bytes) = resume::read_limited(&path, 1 << 30) else { continue };
            match metainfo::parse_torrent(&bytes, limits) {
                Ok(parsed) => {
                    let hash = parsed.meta.info_hash_v1_hex();
                    if hash.is_empty() || out.contains_key(&hash) {
                        continue;
                    }
                    let resume = Some(resumes.join(format!("{hash}.resume"))).filter(|p| p.is_file()).or_else(|| find_legacy(&resumes, &hash, "resume"));
                    out.insert(hash.clone(), Source { tr_hash: hash, torrent: Some((bytes, parsed)), magnet: None, resume });
                }
                Err(e) => rep.failed_torrents.push((name.clone(), format!("Cannot parse torrent info: {e}"))),
            }
        } else if name.ends_with(".magnet") {
            let text = resume::read_limited(&path, 1 << 20).map(|d| String::from_utf8_lossy(&d).trim().to_owned()).unwrap_or_default();
            match metainfo::parse_magnet(&text) {
                Ok(m) => {
                    let hash = m.info_hash_v1.map(|h| metainfo::hex(&h)).unwrap_or_else(|| metainfo::hex(&m.info_hash));
                    if out.contains_key(&hash) {
                        continue;
                    }
                    let resume = Some(resumes.join(format!("{hash}.resume"))).filter(|p| p.is_file());
                    out.insert(hash.clone(), Source { tr_hash: hash, torrent: None, magnet: Some(m), resume });
                }
                Err(e) => rep.failed_torrents.push((name.clone(), format!("invalid magnet link: {e}"))),
            }
        }
    }
    out.into_values().collect()
}

/// Where a file is: (folder, path relative to it) — the name as is or
/// with Transmission's `.part` suffix.
fn locate(dirs: &[&str], rel: &str) -> Option<(String, String, std::fs::Metadata)> {
    for d in dirs.iter().filter(|d| !d.is_empty()) {
        for name in [rel.to_owned(), format!("{rel}.part")] {
            if let Ok(meta) = std::fs::symlink_metadata(Path::new(d).join(&name))
                && meta.is_file()
            {
                return Some(((*d).to_owned(), name, meta));
            }
        }
    }
    None
}

fn mtime(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Context shared by every torrent.
struct Ctx<'a> {
    opts: &'a Options,
    transfers: &'a BTreeMap<String, Transfer>,
    groups: &'a BTreeMap<i64, String>,
    queue: &'a [String],
    limits: Limits,
}

/// Our record of one Transmission torrent, its summary, the engine
/// bitfield (when proven) and the `.torrent` bytes.
/// A converted torrent: our record, its summary and the engine bitfield
/// (engine hash, bits) when the pieces are proven.
type Converted = (FastResume, TorrentSummary, Option<(String, Vec<u8>)>);

fn convert(ctx: &Ctx, src: &Source) -> Result<Converted, String> {
    let r = match &src.resume {
        Some(p) => TrResume::parse(&resume::read_limited(p, 64 * 1024 * 1024).map_err(|e| e.to_string())?, ctx.limits)?,
        None => return Err("no resume data (Resume/<hash>.resume) for this torrent".into()),
    };
    let transfer = ctx.transfers.get(&src.tr_hash);
    let meta = src.torrent.as_ref().map(|(_, p)| &p.meta);
    let id = match (meta, &src.magnet) {
        (Some(m), _) => m.id(),
        (None, Some(mg)) => metainfo::hex(&mg.info_hash),
        _ => return Err("neither metadata nor a magnet link".into()),
    };
    let mut notes = Vec::new();
    let complete_once = r.done_date > 0;
    let mut fr = FastResume {
        info_hash: id.clone(),
        auto_tmm: false,
        save_path: r.destination.clone(),
        download_path: if complete_once { String::new() } else { r.incomplete_dir.clone() },
        added_on: if r.added_date > 0 { r.added_date } else { chrono::Utc::now().timestamp() },
        completed_on: if r.done_date > 0 { r.done_date } else { -1 },
        all_time_download: r.downloaded,
        all_time_upload: r.uploaded,
        active_time: r.seeding_seconds + r.downloading_seconds,
        seeding_time: r.seeding_seconds,
        ratio_limit: match r.ratio_mode {
            1 => r.ratio_limit.max(0.0),
            2 => -1.0,
            _ => -2.0,
        },
        inactive_seeding_time_limit: match r.idle_mode {
            1 => r.idle_limit.max(1),
            2 => -1,
            _ => -2,
        },
        seeding_time_limit: -2,
        share_limit_action: if transfer.is_some_and(|t| t.remove_when_finished_seeding) { "Remove".into() } else { "Default".into() },
        download_limit: r.download_limit.unwrap_or(0),
        upload_limit: r.upload_limit.unwrap_or(0),
        sequential: r.sequential,
        seed_status: complete_once,
        location: r.destination.clone(),
        ..Default::default()
    };
    if r.activity_date > 0 {
        if complete_once {
            fr.last_upload = r.activity_date;
        } else {
            fr.last_download = r.activity_date;
        }
    }
    fr.stopped = match transfer {
        Some(t) => !(t.active || t.wait_to_start),
        None => r.paused,
    };
    if ctx.opts.stop_all {
        fr.stopped = true;
    }
    if let Some(t) = transfer
        && t.group >= 0
    {
        match ctx.groups.get(&t.group) {
            Some(name) => fr.category = name.clone(),
            None => notes.push(format!("group {} no longer exists in Transmission's settings: no category", t.group)),
        }
    }
    for l in &r.labels {
        let l = l.trim();
        if categories::is_valid_tag(l) {
            if !fr.tags.iter().any(|t| t == l) {
                fr.tags.push(l.to_owned());
            }
        } else if !l.is_empty() {
            notes.push(format!("invalid label \"{l}\" dropped"));
        }
    }
    if !r.honors_session_limits {
        notes.push("ignored the global speed limits in Transmission; qBit NeoRS applies them".into());
    }
    if r.bandwidth_priority != 0 {
        notes.push(format!("bandwidth priority {} not carried over", if r.bandwidth_priority > 0 { "high" } else { "low" }));
    }
    if !r.bandwidth_group.is_empty() {
        notes.push("bandwidth group not carried over".into());
    }
    if r.peer_count > 0 {
        notes.push("known peers not carried over".into());
    }
    if fr.share_limit_action == "Remove" {
        notes.push("removed when its seeding limit is reached".into());
    }

    let mut sum = TorrentSummary {
        id: id.clone(),
        category: fr.category.clone(),
        tags: fr.tags.clone(),
        save_path: fr.save_path.clone(),
        uploaded: fr.all_time_upload,
        downloaded: fr.all_time_download,
        ..Default::default()
    };
    let mut bitfield = None;
    match (meta, &src.torrent) {
        (Some(m), Some(_)) => {
            sum.name = if r.name.is_empty() { m.name.clone() } else { r.name.clone() };
            if !r.name.is_empty() && r.name != m.name {
                fr.name = r.name.clone();
            }
            fr.trackers = m.trackers.clone();
            fr.url_seeds = m.url_seeds.clone();
            fr.trusted_urls = fr.trackers.iter().map(|t| t.url.clone()).chain(fr.url_seeds.iter().cloned()).collect();
            // File paths: Transmission's renames, else the metadata's.
            let mut renamed = 0;
            let mut file_paths: Vec<String> = m
                .files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let t = r.files.get(tr_index(r.files.len(), m, i)).map(String::as_str).unwrap_or("");
                    if t.is_empty() {
                        f.path.clone()
                    } else {
                        let p = paths::normalize_user_path(t);
                        if p != f.path {
                            renamed += 1;
                        }
                        p
                    }
                })
                .collect();
            if !file_paths.iter().all(|p| paths::is_valid_relative_path(p)) {
                notes.push("renamed file paths are invalid; the original paths are used".into());
                file_paths = m.file_paths();
                renamed = 0;
            }
            if renamed > 0 {
                notes.push(format!("{renamed} renamed file path(s)"));
            }
            let mut low = 0;
            fr.file_priorities = (0..m.files.len())
                .map(|i| {
                    if r.dnd.get(tr_index(r.dnd.len(), m, i)).copied().unwrap_or(false) {
                        return FilePriority::Ignored.as_i32();
                    }
                    match r.priority.get(tr_index(r.priority.len(), m, i)).copied().unwrap_or(0) {
                        p if p > 0 => FilePriority::High.as_i32(),
                        p => {
                            low += usize::from(p < 0);
                            FilePriority::Normal.as_i32()
                        }
                    }
                })
                .collect();
            if low > 0 {
                notes.push(format!("{low} low-priority file(s) set to normal (qBit NeoRS has no low priority)"));
            }
            sum.files = m.files.len();
            sum.skipped_files = fr.file_priorities.iter().filter(|p| **p == 0).count();

            match r.have_pieces(m) {
                Ok(have) => {
                    let claimed = |i: usize| {
                        let f = &m.files[i];
                        f.size > 0 && (f.first_piece..=f.last_piece).any(|pc| have.get(pc as usize).copied().unwrap_or(false))
                    };
                    // Where the data is: the incomplete folder while downloading.
                    let dirs: Vec<&str> =
                        if complete_once { vec![&r.destination, &r.incomplete_dir] } else { vec![&r.incomplete_dir, &r.destination] };
                    let mut found_dir: Option<String> = None;
                    let (mut missing, mut changed, mut part) = (0usize, 0usize, 0usize);
                    for (i, path) in file_paths.iter_mut().enumerate() {
                        if !claimed(i) {
                            continue;
                        }
                        match locate(&dirs, path) {
                            Some((dir, name, fmeta)) => {
                                if found_dir.is_none() {
                                    found_dir = Some(dir.clone());
                                }
                                if name != *path {
                                    *path = name;
                                    part += 1;
                                }
                                let now = mtime(&fmeta);
                                let proven = if let Some(mt) = &r.mtimes {
                                    let stored = mt.get(tr_index(mt.len(), m, i)).copied().unwrap_or(0);
                                    stored != 0 && stored == now
                                } else if let Some(tc) = &r.time_checked {
                                    let checked = tc.get(tr_index(tc.len(), m, i)).copied().unwrap_or(0);
                                    checked != 0 && now <= checked
                                } else {
                                    false
                                };
                                if !proven {
                                    changed += 1;
                                }
                            }
                            None => missing += 1,
                        }
                    }
                    if let Some(dir) = found_dir {
                        fr.location = dir;
                    }
                    if part > 0 {
                        notes.push(format!("{part} incomplete file(s) keep Transmission's \".part\" name (rename them after they complete)"));
                    }
                    if changed > 0 {
                        sum.pieces = format!(
                            "full recheck: {changed} file(s) changed since Transmission last checked them (or it never recorded when)"
                        );
                    } else {
                        let done = have.iter().filter(|h| **h).count();
                        fr.completed_bytes = super::libtorrent::completed_wanted_bytes(m, &fr.file_priorities, &have);
                        sum.progress = Some(super::libtorrent::wanted_progress(m, &fr.file_priorities, &have));
                        if sum.progress == Some(1.0) {
                            fr.seed_status = true;
                        }
                        sum.pieces = format!("bitfield imported: {done}/{} pieces (no recheck)", have.len());
                        bitfield = Some((m.engine_hash_hex(), super::libtorrent::engine_bitfield(&have)));
                        if missing > 0 {
                            let dir_exists = dirs.iter().any(|d| !d.is_empty() && Path::new(d).is_dir());
                            notes.push(if dir_exists {
                                format!("{missing} file(s) with data are missing on disk: shown as \"Missing Files\"")
                            } else {
                                format!("data folder {} not found (drive not mounted?): shown as \"Missing Files\"", r.destination)
                            });
                        }
                    }
                }
                Err(reason) => sum.pieces = format!("full recheck: {reason}"),
            }
            fr.file_paths = file_paths;
            if fr.location != fr.save_path && !fr.download_path.is_empty() && fr.location != fr.download_path {
                notes.push(format!("data is in {}", fr.location));
            }
        }
        _ => {
            let mg = src.magnet.as_ref().ok_or("no metadata")?;
            sum.name = if r.name.is_empty() { mg.name.clone() } else { r.name.clone() };
            fr.magnet_name = sum.name.clone();
            fr.trackers = mg.trackers.clone();
            fr.url_seeds = mg.url_seeds.clone();
            fr.trusted_urls = fr.trackers.iter().map(|t| t.url.clone()).chain(fr.url_seeds.iter().cloned()).collect();
            if let Some(v2) = mg.info_hash_v2 {
                fr.info_hash_v2 = metainfo::hex(&v2);
                if let Some(v1) = mg.info_hash_v1 {
                    fr.info_hash_v1 = metainfo::hex(&v1);
                }
            }
            sum.pieces = "no metadata yet (magnet): fetched from peers".into();
        }
    }
    // Queue order (4.x queue.json, else Transfers.plist order) for torrents still downloading.
    fr.queue_position = if fr.seed_status { -1 } else { ctx.queue.iter().position(|h| *h == src.tr_hash).map(|p| p as i64).unwrap_or(-1) };
    sum.state = if fr.stopped { "stopped".into() } else { "started".into() };
    sum.location = fr.location.clone();
    sum.trackers = fr.trackers.len();
    sum.web_seeds = fr.url_seeds.len();
    sum.notes = notes;
    Ok((fr, sum, bitfield))
}

/// What a merge already imported (`migration/transmission-merge.json`).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct MergeMarker {
    stats_added: bool,
    torrents: Vec<String>,
}

/// Transmission's statistics, dates, limits left at their defaults and
/// queue position for a torrent the profile already has. Paths, category,
/// tags, priorities, state and pieces stay as they are.
fn merge_into(mine: &mut FastResume, theirs: &FastResume, already: bool, stop_all: bool) -> Vec<String> {
    let mut notes = Vec::new();
    if already {
        notes.push("statistics already merged by an earlier run".into());
    } else {
        mine.all_time_upload = mine.all_time_upload.max(0) + theirs.all_time_upload;
        mine.all_time_download = mine.all_time_download.max(0) + theirs.all_time_download;
        mine.active_time = mine.active_time.max(0) + theirs.active_time;
        mine.seeding_time = mine.seeding_time.max(0) + theirs.seeding_time;
        notes.push("Transmission's uploaded/downloaded totals, active and seeding time added".into());
    }
    if theirs.added_on > 0 && (mine.added_on <= 0 || theirs.added_on < mine.added_on) {
        mine.added_on = theirs.added_on;
    }
    if theirs.completed_on > 0 {
        mine.completed_on = theirs.completed_on;
    }
    mine.last_upload = mine.last_upload.max(theirs.last_upload);
    mine.last_download = mine.last_download.max(theirs.last_download);
    if mine.download_limit <= 0 {
        mine.download_limit = theirs.download_limit;
    }
    if mine.upload_limit <= 0 {
        mine.upload_limit = theirs.upload_limit;
    }
    if mine.ratio_limit == -2.0 {
        mine.ratio_limit = theirs.ratio_limit;
    }
    if mine.inactive_seeding_time_limit == -2 {
        mine.inactive_seeding_time_limit = theirs.inactive_seeding_time_limit;
    }
    if mine.share_limit_action == "Default" {
        mine.share_limit_action = theirs.share_limit_action.clone();
    }
    for t in &theirs.trackers {
        if !mine.trackers.iter().any(|x| x.url == t.url) {
            let tier = mine.trackers.iter().map(|x| x.tier + 1).max().unwrap_or(0);
            mine.trackers.push(TrackerTierEntry { url: t.url.clone(), tier });
            mine.trusted_urls.push(t.url.clone());
        }
    }
    if theirs.queue_position >= 0 {
        mine.queue_position = theirs.queue_position;
    }
    if stop_all {
        mine.stopped = true;
    }
    if mine.category != theirs.category && !theirs.category.is_empty() {
        notes.push(format!("category differs (profile \"{}\", Transmission \"{}\"): kept", mine.category, theirs.category));
    }
    let (mut a, mut b) = (mine.tags.clone(), theirs.tags.clone());
    a.sort();
    b.sort();
    if a != b && !b.is_empty() {
        notes.push(format!("tags differ (profile [{}], Transmission labels [{}]): kept", mine.tags.join(", "), theirs.tags.join(", ")));
    }
    if !mine.auto_tmm && mine.save_path.trim_end_matches('/') != theirs.save_path.trim_end_matches('/') {
        notes.push(format!("save path differs (profile {}, Transmission {}): kept", mine.save_path, theirs.save_path));
    }
    if !theirs.file_priorities.is_empty() && mine.file_priorities != theirs.file_priorities {
        notes.push("file priorities differ: kept".into());
    }
    notes
}

// ---------------------------------------------------------------- run

/// Whether a profile already has torrents.
pub fn profile_has_torrents(profile: &Path) -> bool {
    std::fs::read_dir(profile.join("data/BT_backup"))
        .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".fastresume")))
        .unwrap_or(false)
}

/// Runs the migration (or only plans it with `dry_run`).
pub fn run(opts: &Options) -> Result<Report, String> {
    let mut rep = Report {
        source_app: "Transmission".into(),
        dry_run: opts.dry_run,
        profile: opts.profile.display().to_string(),
        sandbox: opts.sandbox.clone(),
        ..Default::default()
    };

    // ---- safety checks
    if super::is_inside(&opts.profile, &opts.source_dir) || (opts.profile.exists() && super::is_inside(&opts.source_dir, &opts.profile)) {
        return Err(format!("the profile folder must be separate from Transmission's folder {}", opts.source_dir.display()));
    }
    if !opts.source_dir.join("Torrents").is_dir() && !opts.source_dir.join("Resume").is_dir() {
        rep.warnings.push(format!("{} has no Torrents or Resume folder: no torrents to migrate", opts.source_dir.display()));
    }
    for (what, who) in [("the profile is in use by", super::profile_in_use(&opts.profile)), ("Transmission is running as", transmission_running())] {
        if let Some(who) = who {
            let msg = format!("{what} {who}: quit it first");
            if opts.dry_run {
                rep.warnings.push(msg);
            } else {
                return Err(msg);
            }
        }
    }
    if opts.merge {
        if !opts.profile.join("config/preferences.json").is_file() {
            return Err(format!("{} has no qBit NeoRS profile to merge into; migrate without --merge", opts.profile.display()));
        }
    } else if !opts.dry_run && !opts.force && (opts.profile.join("config/preferences.json").exists() || profile_has_torrents(&opts.profile)) {
        return Err(format!(
            "{} already holds a qBit NeoRS profile; move it away first (or pass --force to write over it, or --merge)",
            opts.profile.display()
        ));
    }
    rep.sources.push(format!("data: {}", opts.source_dir.display()));

    // ---- settings
    let defaults = read_defaults(opts, &mut rep);
    let mapped = map_settings(&defaults);
    let mut prefs = mapped.prefs;
    rep.prefs_source = if defaults.as_dict().is_some_and(|d| !d.is_empty()) { "Transmission's settings".into() } else { "Transmission's defaults".into() };
    rep.prefs_notes = mapped.notes;
    rep.ini_unused = mapped.unmapped;
    rep.remote_access_enabled = mapped.remote_access;
    rep.watched_folders = prefs.downloads.watched_folders.len();
    let target_prefs: Option<Preferences> = if opts.merge {
        let data = resume::read_limited(&opts.profile.join("config/preferences.json"), MAX_CONFIG).map_err(|e| e.to_string())?;
        Some(serde_json::from_slice(&data).map_err(|e| format!("config/preferences.json: {e}"))?)
    } else {
        None
    };
    let groups = match parse_groups(&defaults) {
        Ok(g) => g,
        Err(e) => {
            rep.warnings.push(format!("groups could not be read ({e}): no categories from groups"));
            Vec::new()
        }
    };
    if groups.iter().any(|g| g.has_rules) {
        rep.ini_unused
            .entry("no equivalent".into())
            .or_default()
            .push("automatic group assignment rules (assign categories by hand, or use RSS/watched folder rules)".into());
    }

    // ---- app state
    let (transfers, plist_order) = match super::read_opt(&opts.source_dir.join("Transfers.plist"), MAX_CONFIG).map(|d| parse_transfers(&d)) {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            rep.warnings.push(format!("Transfers.plist: {e}; started/stopped state taken from the resume data, no groups"));
            Default::default()
        }
        None => Default::default(),
    };
    let queue = match super::read_opt(&opts.source_dir.join("queue.json"), MAX_CONFIG) {
        Some(d) => parse_queue(&d),
        None => plist_order,
    };

    // ---- categories and tags
    let mut cats: CategoryMap = if opts.merge {
        super::read_opt(&opts.profile.join("config/categories.json"), MAX_CONFIG).and_then(|d| categories::from_json(&d).ok()).unwrap_or_default()
    } else {
        CategoryMap::new()
    };
    let mut group_names: BTreeMap<i64, String> = BTreeMap::new();
    let used_groups: BTreeSet<i64> = transfers.values().map(|t| t.group).filter(|g| *g >= 0).collect();
    for g in &groups {
        let name = g.name.replace('\\', "/");
        let name = name.trim_matches('/').to_owned();
        if !categories::is_valid_category_name(&name) {
            if used_groups.contains(&g.index) || g.custom_location.is_some() {
                rep.warnings.push(format!("group \"{}\" has no valid category name; its torrents get no category", g.name));
            }
            continue;
        }
        group_names.insert(g.index, name.clone());
        if !used_groups.contains(&g.index) && g.custom_location.is_none() {
            continue;
        }
        if !cats.contains_key(&name) {
            for c in categories::expand_category(&name) {
                cats.entry(c.clone()).or_insert_with(|| categories::new_category(&c));
            }
            if let Some(loc) = &g.custom_location
                && let Some(c) = cats.get_mut(&name)
            {
                c.save_path = loc.clone();
            }
        }
    }
    let mut tags: BTreeSet<String> = if opts.merge { resume::load_json::<Vec<String>>(&opts.profile.join("config/tags.json")).into_iter().collect() } else { BTreeSet::new() };

    // ---- torrents
    let limits = Limits { max_depth: prefs.advanced.bdecode_depth_limit.max(100) as usize, max_tokens: prefs.advanced.bdecode_token_limit.max(10_000_000) as usize };
    let sources = scan_sources(&opts.source_dir, limits, &mut rep);
    let ctx = Ctx { opts, transfers: &transfers, groups: &group_names, queue: &queue, limits };
    let mut writer = Writer { root: opts.profile.clone(), dry_run: opts.dry_run, merge: opts.merge, written: Vec::new() };
    let backup = BackupDir::new(opts.profile.join("data/BT_backup"));
    let marker_path = opts.profile.join("migration/transmission-merge.json");
    let mut marker: MergeMarker = if opts.merge { resume::load_json(&marker_path) } else { MergeMarker::default() };
    for src in &sources {
        let (fr, mut sum, bitfield) = match convert(&ctx, src) {
            Ok(x) => x,
            Err(e) => {
                rep.failed_torrents.push((src.tr_hash.clone(), e));
                continue;
            }
        };
        if opts.merge && backup.resume_path(&fr.info_hash).is_file() {
            let data = resume::read_limited(&backup.resume_path(&fr.info_hash), 64 * 1024 * 1024).map_err(|e| e.to_string())?;
            let mut mine: FastResume = match serde_json::from_slice(&data) {
                Ok(m) => m,
                Err(e) => {
                    rep.failed_torrents.push((fr.info_hash.clone(), format!("profile record: {e}")));
                    continue;
                }
            };
            let already = marker.torrents.contains(&fr.info_hash);
            let notes = merge_into(&mut mine, &fr, already, opts.stop_all);
            if !already {
                marker.torrents.push(fr.info_hash.clone());
            }
            if !opts.dry_run {
                backup.save_resume(&mine).map_err(|e| e.to_string())?;
            }
            writer.written.push(format!("data/BT_backup/{}.fastresume", fr.info_hash));
            sum.state = format!("merged ({})", if mine.stopped { "stopped" } else { "state kept" });
            sum.pieces = "pieces kept (no recheck)".into();
            sum.progress = None;
            sum.category = mine.category.clone();
            sum.tags = mine.tags.clone();
            sum.save_path = mine.save_path.clone();
            sum.uploaded = mine.all_time_upload;
            sum.downloaded = mine.all_time_download;
            sum.notes = notes;
            rep.torrents.push(sum);
            continue;
        }
        if !fr.category.is_empty() && !cats.contains_key(&fr.category) {
            for c in categories::expand_category(&fr.category) {
                cats.entry(c.clone()).or_insert_with(|| categories::new_category(&c));
            }
        }
        tags.extend(fr.tags.iter().cloned());
        if let Some((engine_hash, bits)) = &bitfield {
            writer.write(&format!("data/engine/fastresume/{engine_hash}.bitv"), bits)?;
        }
        if let Some((bytes, _)) = &src.torrent {
            if !opts.dry_run {
                backup.save_torrent_file(&fr.info_hash, bytes).map_err(|e| e.to_string())?;
            }
            writer.written.push(format!("data/BT_backup/{}.torrent", fr.info_hash));
        }
        if !opts.dry_run {
            backup.save_resume(&fr).map_err(|e| e.to_string())?;
        }
        writer.written.push(format!("data/BT_backup/{}.fastresume", fr.info_hash));
        rep.torrents.push(sum);
    }
    rep.categories = cats.keys().cloned().collect();
    rep.tags = tags.iter().cloned().collect();
    writer.write("config/categories.json", &categories::to_json(&cats))?;
    let tag_list: Vec<&String> = tags.iter().collect();
    writer.write("config/tags.json", &serde_json::to_vec_pretty(&tag_list).map_err(|e| e.to_string())?)?;

    // ---- statistics, session state
    if let Some((dl, ul)) = super::read_opt(&opts.source_dir.join("stats.json"), MAX_CONFIG).and_then(|d| parse_stats(&d)) {
        let mut stats = StatsFile { alltime_dl: dl, alltime_ul: ul };
        rep.stats = Some((dl, ul));
        if opts.merge {
            let mine: StatsFile = resume::load_json(&opts.profile.join("data/stats.json"));
            if marker.stats_added {
                stats = mine;
            } else {
                stats.alltime_dl += mine.alltime_dl.max(0);
                stats.alltime_ul += mine.alltime_ul.max(0);
                marker.stats_added = true;
            }
        }
        writer.write("data/stats.json", &serde_json::to_vec_pretty(&stats).map_err(|e| e.to_string())?)?;
    }
    let state = SessionStateFile { use_alt_speed_limits: mapped.alt_speed };
    writer.write_new("data/session_state.json", &serde_json::to_vec_pretty(&state).map_err(|e| e.to_string())?)?;
    for (what, path) in [("DHT routing table (dht.dat)", "dht.dat"), ("blocklists", "blocklists")] {
        let p = opts.source_dir.join(path);
        let has = if p.is_dir() { std::fs::read_dir(&p).map(|mut d| d.next().is_some()).unwrap_or(false) } else { p.is_file() };
        if has {
            rep.ini_unused.entry("not carried over".into()).or_default().push(format!("{what}: rebuilt by qBit NeoRS"));
        }
    }

    // ---- GUI settings
    rep.gui_keys = mapped.gui.iter().map(|(k, _)| k.clone()).collect();
    let plist_rel = if opts.merge || opts.gui_plist_in_migration { "migration/gui-preferences.plist" } else { "config/gui-preferences.plist" };
    writer.write(plist_rel, super::gui::to_plist(&mapped.gui).as_bytes())?;
    let mut keys = rep.gui_keys.join("\n");
    keys.push('\n');
    writer.write("migration/gui-preferences.keys", keys.as_bytes())?;

    // ---- preferences
    if let Some(t) = target_prefs {
        prefs = t;
        if prefs.downloads.incomplete_files_ext && rep.torrents.iter().any(|t| t.notes.iter().any(|n| n.contains(".part"))) {
            rep.warnings.push("the profile appends \".!qB\" to incomplete files: Transmission's \".part\" files of new torrents are then not found".into());
        }
        if let Some(sb) = &opts.sandbox {
            super::apply_sandbox(&mut prefs, sb);
            writer.write("config/preferences.json", &serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?)?;
        }
        writer.write("migration/transmission-merge.json", &serde_json::to_vec_pretty(&marker).map_err(|e| e.to_string())?)?;
    } else {
        if let Some(sb) = &opts.sandbox {
            super::apply_sandbox(&mut prefs, sb);
        }
        writer.write("config/preferences.json", &serde_json::to_vec_pretty(&prefs).map_err(|e| e.to_string())?)?;
    }

    writer.written.push("migration/report.txt".into());
    rep.files_written = writer.written.clone();
    let text = rep.to_text();
    if !opts.dry_run {
        resume::write_private_atomic(&opts.profile.join("migration/report.txt"), text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(rep)
}


#[cfg(test)]
#[path = "transmission_tests.rs"]
mod tests;
