//! The Lua API (qBittorrent 5.3 `luafunctions.cpp`, `luaclasses.cpp`): the
//! `qBittorrent` namespace and the torrent object handed to event handlers.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mlua::{Lua, MultiValue, Table, UserData, UserDataFields, UserDataMethods, Value};

use super::host::{PluginHost, ShareLimitsValue, TorrentAction};
use super::runtime::Budget;
use super::{PluginArg, TorrentArg, datetime};
use crate::bittorrent::autorun;
use crate::logger::Logger;
use crate::model::{ShareLimitAction, ShareLimitsMode, TorrentInfo, TorrentState, TrackerEntry, TrackerStatus};

/// Log lines a plugin may write per callback.
pub(crate) const MAX_LOG_LINES: u32 = 200;
/// Programs a plugin may start per callback.
pub(crate) const MAX_EXECS: u32 = 8;
/// E-mails a plugin may send per callback.
pub(crate) const MAX_MAILS: u32 = 4;
/// Programs started by plugins that may run at the same time.
const MAX_RUNNING_PROGRAMS: usize = 16;
/// A program started by a plugin is killed after this time.
pub(crate) const PROGRAM_TIME_LIMIT: Duration = Duration::from_secs(10 * 60);

static RUNNING_PROGRAMS: AtomicUsize = AtomicUsize::new(0);

/// Per-plugin counters kept while the engine runs.
#[derive(Default)]
pub(crate) struct Stats {
    pub programs_run: Cell<i64>,
    pub mails_sent: Cell<i64>,
}

/// What the API functions of one Lua state need (stored as app data).
pub(crate) struct PluginCtx {
    pub id: String,
    pub host: Arc<dyn PluginHost>,
    pub logger: Arc<Logger>,
    pub rt: tokio::runtime::Handle,
    pub budget: Rc<Budget>,
    pub stats: Rc<Stats>,
}

fn ctx(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, PluginCtx>> {
    lua.app_data_ref::<PluginCtx>().ok_or_else(|| mlua::Error::runtime("plugin context missing"))
}

/// Registers the `qBittorrent` namespace (after the main chunk ran, as in
/// qBittorrent, so top-level code cannot use it).
pub(crate) fn register(lua: &Lua) -> mlua::Result<()> {
    let ns = lua.create_table()?;

    ns.set("debug", lua.create_function(|lua, message: String| {
        let c = ctx(lua)?;
        tracing::debug!(plugin = %c.id, "{message}");
        Ok(())
    })?)?;

    ns.set("log", lua.create_function(|lua, message: String| {
        let c = ctx(lua)?;
        let n = c.budget.log_lines.get() + 1;
        c.budget.log_lines.set(n);
        if n <= MAX_LOG_LINES {
            c.logger.normal(message);
        } else if !c.budget.log_overflow_reported.replace(true) {
            c.logger.warning(format!("Plugin '{}' wrote too many log lines; the rest of this call is not logged.", c.id));
        }
        Ok(())
    })?)?;

    ns.set("exec", lua.create_function(|lua, command: String| {
        let c = ctx(lua)?;
        Ok(exec(&c, &command))
    })?)?;

    ns.set("sendMail", lua.create_function(|lua, (subject, body): (String, String)| {
        let c = ctx(lua)?;
        let n = c.budget.mails.get() + 1;
        c.budget.mails.set(n);
        if n > MAX_MAILS {
            c.logger.warning(format!("Plugin '{}' tried to send too many e-mails at once; not sent.", c.id));
            return Ok(());
        }
        c.stats.mails_sent.set(c.stats.mails_sent.get() + 1);
        c.host.send_mail(&c.id, subject, body);
        Ok(())
    })?)?;

    ns.set("friendlySizeUnit", lua.create_function(|_, (bytes, precision): (i64, Option<i64>)| {
        Ok(friendly_unit(bytes, false, precision.unwrap_or(-1)))
    })?)?;
    ns.set("friendlySpeedUnit", lua.create_function(|_, (bytes, precision): (i64, Option<i64>)| {
        Ok(friendly_unit(bytes, true, precision.unwrap_or(-1)))
    })?)?;
    ns.set("friendlyDuration", lua.create_function(|_, seconds: i64| {
        Ok(crate::utils::misc::user_friendly_duration(seconds, -1))
    })?)?;
    ns.set("formatDateTime", lua.create_function(|_, args: MultiValue| {
        let mut it = args.into_iter();
        match (it.next(), it.next()) {
            (Some(Value::String(format)), Some(time)) => {
                Ok(datetime::format_qt(&format.to_str()?, to_int(&time)?))
            }
            (Some(time), None) => Ok(datetime::format_short(to_int(&time)?)),
            _ => Err(mlua::Error::runtime("formatDateTime: expected ([format,] time)")),
        }
    })?)?;

    // Value classes with default constructors.
    ns.set("ShareLimits", lua.create_function(|lua, ()| share_limits_table(lua, &ShareLimitsValue::default()))?)?;
    ns.set("TrackerEntry", lua.create_function(|lua, ()| {
        let t = lua.create_table()?;
        t.set("url", "")?;
        t.set("tier", 0)?;
        Ok(t)
    })?)?;

    let states = lua.create_table()?;
    for (k, v) in [("NotContacted", 1), ("Working", 2), ("NotWorking", 4), ("TrackerError", 5), ("Unreachable", 6)] {
        states.set(k, v)?;
    }
    ns.set("TrackerEndpointState", states)?;
    let actions = lua.create_table()?;
    for (k, v) in [("Default", -1), ("Stop", 0), ("Remove", 1), ("RemoveWithContent", 3), ("EnableSuperSeeding", 2)] {
        actions.set(k, v)?;
    }
    ns.set("ShareLimitAction", actions)?;
    let modes = lua.create_table()?;
    for (k, v) in [("Default", -1), ("MatchAny", 0), ("MatchAll", 1)] {
        modes.set(k, v)?;
    }
    ns.set("ShareLimitsMode", modes)?;

    lua.globals().set("qBittorrent", ns)
}

fn to_int(v: &Value) -> mlua::Result<i64> {
    match v {
        Value::Integer(i) => Ok(*i),
        Value::Number(n) if n.is_finite() => Ok(*n as i64),
        Value::String(s) => s.to_str()?.trim().parse().map_err(|_| mlua::Error::runtime("number expected")),
        _ => Err(mlua::Error::runtime("number expected")),
    }
}

/// `Utils::Misc::friendlyUnit(bytes, isSpeed, precision)`; precision -1
/// uses the unit's default.
fn friendly_unit(bytes: i64, is_speed: bool, precision: i64) -> String {
    if precision < 0 {
        return crate::utils::misc::friendly_unit(bytes, is_speed);
    }
    if bytes < 0 {
        return "Unknown".to_string();
    }
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let digits = if unit == 0 { 0 } else { precision.min(9) as usize };
    let mut s = crate::utils::misc::from_double(value, digits);
    s.push('\u{a0}');
    s.push_str(UNITS[unit]);
    if is_speed {
        s.push_str("/s");
    }
    s
}

/// `qBittorrent.exec`: runs a program without a shell. The command line is
/// split like qBittorrent does on Unix (`Utils::String::splitCommand`,
/// redundant quotes stripped); output goes nowhere; the program is killed
/// after [`PROGRAM_TIME_LIMIT`]. Every run is logged.
fn exec(c: &PluginCtx, command: &str) -> bool {
    let n = c.budget.execs.get() + 1;
    c.budget.execs.set(n);
    if n > MAX_EXECS {
        c.logger.warning(format!("Plugin '{}' tried to run too many programs at once. Command: `{command}`", c.id));
        return false;
    }
    let argv = autorun::split_program(command);
    if argv.first().is_none_or(|p| p.is_empty()) {
        return false;
    }
    if RUNNING_PROGRAMS.load(Ordering::Relaxed) >= MAX_RUNNING_PROGRAMS {
        c.logger.warning(format!("Plugin '{}' couldn't run a program: too many are running. Command: `{command}`", c.id));
        return false;
    }
    let _guard = c.rt.enter();
    let started = autorun::spawn_limited(&argv, PROGRAM_TIME_LIMIT, &RUNNING_PROGRAMS);
    if started {
        c.stats.programs_run.set(c.stats.programs_run.get() + 1);
        c.logger.info(format!("Plugin '{}' is running external program. Command: `{command}`", c.id));
    } else {
        c.logger.warning(format!("Plugin '{}' failed to run external program. Command: `{command}`", c.id));
    }
    started
}

// ---- Conversions ------------------------------------------------------------

fn mode_to_int(m: ShareLimitsMode) -> i64 {
    match m {
        ShareLimitsMode::Default => -1,
        ShareLimitsMode::MatchAny => 0,
        ShareLimitsMode::MatchAll => 1,
    }
}

fn int_to_mode(v: i64) -> ShareLimitsMode {
    match v {
        0 => ShareLimitsMode::MatchAny,
        1 => ShareLimitsMode::MatchAll,
        _ => ShareLimitsMode::Default,
    }
}

fn share_limits_table(lua: &Lua, l: &ShareLimitsValue) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("ratioLimit", l.ratio_limit)?;
    t.set("seedingTimeLimit", l.seeding_time_limit)?;
    t.set("inactiveSeedingTimeLimit", l.inactive_seeding_time_limit)?;
    t.set("mode", mode_to_int(l.mode))?;
    t.set("action", i64::from(l.action.as_i32()))?;
    Ok(t)
}

fn share_limits_from(t: &Table) -> mlua::Result<ShareLimitsValue> {
    let d = ShareLimitsValue::default();
    Ok(ShareLimitsValue {
        ratio_limit: t.get::<Option<f64>>("ratioLimit")?.unwrap_or(d.ratio_limit),
        seeding_time_limit: t.get::<Option<i64>>("seedingTimeLimit")?.unwrap_or(d.seeding_time_limit),
        inactive_seeding_time_limit: t.get::<Option<i64>>("inactiveSeedingTimeLimit")?.unwrap_or(d.inactive_seeding_time_limit),
        mode: int_to_mode(t.get::<Option<i64>>("mode")?.unwrap_or(-1)),
        action: ShareLimitAction::from_i32(t.get::<Option<i64>>("action")?.unwrap_or(-1) as i32),
    })
}

/// `TrackerEndpointState` of a tracker status.
fn endpoint_state(s: TrackerStatus) -> i64 {
    match s {
        TrackerStatus::Working => 2,
        TrackerStatus::NotWorking => 4,
        TrackerStatus::TrackerError => 5,
        TrackerStatus::Unreachable => 6,
        TrackerStatus::Disabled | TrackerStatus::NotContacted | TrackerStatus::Updating => 1,
    }
}

/// A `TrackerEntryStatus` object.
fn tracker_status_table(lua: &Lua, t: &TrackerEntry) -> mlua::Result<Table> {
    let o = lua.create_table()?;
    o.set("url", t.url.as_str())?;
    o.set("tier", t.tier)?;
    o.set("updating", t.updating || t.status == TrackerStatus::Updating)?;
    o.set("state", endpoint_state(t.status))?;
    o.set("message", t.message.as_str())?;
    o.set("numSeeds", t.num_seeds)?;
    o.set("numPeers", t.num_peers)?;
    o.set("numLeeches", t.num_leeches)?;
    o.set("numDownloaded", t.num_downloaded)?;
    let endpoints = lua.create_table()?;
    for (i, ep) in t.endpoints.iter().enumerate() {
        let e = lua.create_table()?;
        e.set("name", ep.name.as_str())?;
        e.set("btVersion", ep.bt_version)?;
        e.set("updating", ep.updating || ep.status == TrackerStatus::Updating)?;
        e.set("state", endpoint_state(ep.status))?;
        e.set("message", ep.message.as_str())?;
        e.set("numSeeds", ep.num_seeds)?;
        e.set("numPeers", ep.num_peers)?;
        e.set("numLeeches", ep.num_leeches)?;
        e.set("numDownloaded", ep.num_downloaded)?;
        e.set("nextAnnounceTime", ep.next_announce)?;
        e.set("minAnnounceTime", ep.min_announce)?;
        endpoints.raw_set(i + 1, e)?;
    }
    o.set("endpoints", endpoints)?;
    Ok(o)
}

fn string_list(lua: &Lua, items: &[String]) -> mlua::Result<Table> {
    lua.create_sequence_from(items.iter().map(String::as_str))
}

/// Handler arguments as Lua values.
pub(crate) fn to_lua_args(lua: &Lua, args: Vec<PluginArg>) -> mlua::Result<MultiValue> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        out.push(match a {
            PluginArg::Torrent(t) => Value::UserData(lua.create_userdata(LuaTorrent(t))?),
            PluginArg::Str(s) => Value::String(lua.create_string(&s)?),
            PluginArg::Int(i) => Value::Integer(i),
            PluginArg::StrList(l) => Value::Table(string_list(lua, &l)?),
            PluginArg::IntList(l) => Value::Table(lua.create_sequence_from(l)?),
            PluginArg::IndexedPaths(l) => {
                let t = lua.create_table()?;
                for (i, p) in l {
                    t.raw_set(i, p)?;
                }
                Value::Table(t)
            }
            PluginArg::Trackers(l) => {
                let t = lua.create_table()?;
                for (i, e) in l.iter().enumerate() {
                    let o = lua.create_table()?;
                    o.set("url", e.url.as_str())?;
                    o.set("tier", e.tier)?;
                    t.raw_set(i + 1, o)?;
                }
                Value::Table(t)
            }
            PluginArg::TrackerStatuses(l) => {
                let t = lua.create_table()?;
                for e in &l {
                    t.raw_set(e.url.as_str(), tracker_status_table(lua, e)?)?;
                }
                Value::Table(t)
            }
            PluginArg::Torrents(l) => {
                let t = lua.create_table()?;
                for (i, tor) in l.into_iter().enumerate() {
                    t.raw_set(i + 1, lua.create_userdata(LuaTorrent(tor))?)?;
                }
                Value::Table(t)
            }
        });
    }
    Ok(MultiValue::from_vec(out))
}

// ---- The torrent object -----------------------------------------------------

/// `qBittorrent.Torrent`: reads go to the live torrent (or the copy taken
/// when the event happened once it is gone); writes change it.
pub(crate) struct LuaTorrent(pub TorrentArg);

impl LuaTorrent {
    fn info(&self, lua: &Lua) -> mlua::Result<Arc<TorrentInfo>> {
        let c = ctx(lua)?;
        if let Some(i) = c.host.torrent_info(&self.0.id) {
            return Ok(Arc::new(i));
        }
        self.0.captured.clone().ok_or_else(|| mlua::Error::runtime("The torrent no longer exists."))
    }

    fn apply(&self, lua: &Lua, action: TorrentAction) -> mlua::Result<bool> {
        let c = ctx(lua)?;
        Ok(c.host.apply(&self.0.id, action))
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

macro_rules! info_field {
    ($fields:ident, $name:literal, |$i:ident| $e:expr) => {
        $fields.add_field_method_get($name, |lua, this| {
            let $i = this.info(lua)?;
            Ok($e)
        });
    };
}

impl UserData for LuaTorrent {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        info_field!(fields, "id", |i| i.hash.clone());
        info_field!(fields, "infoHashV1", |i| i.infohash_v1.clone());
        info_field!(fields, "infoHashV2", |i| i.infohash_v2.clone());
        info_field!(fields, "hasMetadata", |i| i.has_metadata);
        info_field!(fields, "name", |i| i.name.clone());
        info_field!(fields, "creationTime", |i| i.creation_date);
        info_field!(fields, "creator", |i| i.created_by.clone());
        info_field!(fields, "comment", |i| i.comment.clone());
        fields.add_field_method_get("filesCount", |lua, this| {
            let i = this.info(lua)?;
            if !i.has_metadata {
                return Ok(-1);
            }
            Ok(ctx(lua)?.host.files(&this.0.id).len() as i64)
        });
        info_field!(fields, "pieceSize", |i| i.piece_size);
        info_field!(fields, "piecesCount", |i| i.pieces_num);
        info_field!(fields, "totalSize", |i| i.total_size);
        info_field!(fields, "private", |i| i.private.unwrap_or(false));
        info_field!(fields, "savePath", |i| i.save_path.clone());
        info_field!(fields, "downloadPath", |i| i.download_path.clone());
        info_field!(fields, "category", |i| i.category.clone());
        fields.add_field_method_get("tags", |lua, this| {
            let i = this.info(lua)?;
            string_list(lua, &i.tags)
        });
        info_field!(fields, "addedTime", |i| i.added_on);
        info_field!(fields, "completedTime", |i| i.completion_on);
        info_field!(fields, "lastSeenComplete", |i| i.last_seen_complete);
        info_field!(fields, "activeTime", |i| i.time_active);
        info_field!(fields, "finishedTime", |i| i.seeding_time);
        info_field!(fields, "timeSinceActivity", |i| since(i.last_activity));
        info_field!(fields, "timeSinceDownload", |i| since(i.last_activity));
        info_field!(fields, "timeSinceUpload", |i| since(i.last_activity));
        info_field!(fields, "wantedSize", |i| i.size);
        info_field!(fields, "completedSize", |i| i.completed);
        info_field!(fields, "wastedSize", |i| i.total_wasted);
        info_field!(fields, "piecesHave", |i| i.pieces_have);
        info_field!(fields, "progress", |i| i.progress);
        info_field!(fields, "rootPath", |i| i.root_path.clone());
        info_field!(fields, "contentPath", |i| i.content_path.clone());
        info_field!(fields, "currentTracker", |i| i.tracker.clone());
        fields.add_field_method_get("trackerStatuses", |lua, this| {
            let list = ctx(lua)?.host.trackers(&this.0.id);
            let t = lua.create_table()?;
            for (n, e) in list.iter().enumerate() {
                t.raw_set(n + 1, tracker_status_table(lua, e)?)?;
            }
            Ok(t)
        });
        fields.add_field_method_get("urlSeeds", |lua, this| {
            let list = ctx(lua)?.host.url_seeds(&this.0.id);
            string_list(lua, &list)
        });
        fields.add_field_method_get("shareLimits", |lua, this| {
            let i = this.info(lua)?;
            share_limits_table(
                lua,
                &ShareLimitsValue {
                    ratio_limit: i.ratio_limit,
                    seeding_time_limit: i.seeding_time_limit,
                    inactive_seeding_time_limit: i.inactive_seeding_time_limit,
                    mode: i.share_limits_mode,
                    action: i.share_limit_action,
                },
            )
        });
        fields.add_field_method_get("effectiveShareLimits", |lua, this| {
            let i = this.info(lua)?;
            let (global_mode, global_action) = ctx(lua)?.host.global_share_limits();
            let mode = if i.share_limits_mode == ShareLimitsMode::Default { global_mode } else { i.share_limits_mode };
            let action = if i.share_limit_action == ShareLimitAction::Default { global_action } else { i.share_limit_action };
            share_limits_table(
                lua,
                &ShareLimitsValue {
                    ratio_limit: i.max_ratio,
                    seeding_time_limit: i.max_seeding_time,
                    inactive_seeding_time_limit: i.max_inactive_seeding_time,
                    mode,
                    action,
                },
            )
        });
        info_field!(fields, "hasMissingFiles", |i| i.state == TorrentState::MissingFiles);
        info_field!(fields, "hasError", |i| i.state == TorrentState::Error);
        info_field!(fields, "error", |i| i.error_message.clone());
        info_field!(fields, "queuePosition", |i| if i.queue_position > 0 { i.queue_position - 1 } else { -1 });

        fields.add_field_method_set("name", |lua, this, v: String| this.apply(lua, TorrentAction::Rename(v)).map(drop));
        fields.add_field_method_set("comment", |lua, this, v: String| this.apply(lua, TorrentAction::SetComment(v)).map(drop));
        fields.add_field_method_set("savePath", |lua, this, v: String| this.apply(lua, TorrentAction::SetSavePath(v)).map(drop));
        fields.add_field_method_set("downloadPath", |lua, this, v: String| {
            this.apply(lua, TorrentAction::SetDownloadPath(v)).map(drop)
        });
        fields.add_field_method_set("category", |lua, this, v: String| this.apply(lua, TorrentAction::SetCategory(v)).map(drop));
        fields.add_field_method_set("shareLimits", |lua, this, v: Table| {
            let limits = share_limits_from(&v)?;
            this.apply(lua, TorrentAction::SetShareLimits(limits)).map(drop)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("filePath", |lua, this, index: i64| {
            let files = ctx(lua)?.host.files(&this.0.id);
            Ok(usize::try_from(index).ok().and_then(|i| files.get(i)).map(|f| f.0.clone()).unwrap_or_default())
        });
        methods.add_method("fileSize", |lua, this, index: i64| {
            let files = ctx(lua)?.host.files(&this.0.id);
            Ok(usize::try_from(index).ok().and_then(|i| files.get(i)).map(|f| f.1).unwrap_or(-1))
        });
        methods.add_method("hasTag", |lua, this, tag: String| Ok(this.info(lua)?.tags.contains(&tag)));
        methods.add_method("addTag", |lua, this, tag: String| {
            if this.info(lua)?.tags.contains(&tag) {
                return Ok(false);
            }
            this.apply(lua, TorrentAction::AddTag(tag))
        });
        methods.add_method("removeTag", |lua, this, tag: String| {
            if !this.info(lua)?.tags.contains(&tag) {
                return Ok(false);
            }
            this.apply(lua, TorrentAction::RemoveTag(tag))
        });
        methods.add_method("clearTags", |lua, this, ()| this.apply(lua, TorrentAction::ClearTags).map(drop));
        methods.add_method("start", |lua, this, ()| this.apply(lua, TorrentAction::Start).map(drop));
        methods.add_method("stop", |lua, this, ()| this.apply(lua, TorrentAction::Stop).map(drop));
    }
}

/// Seconds since a Unix time, -1 when never.
fn since(t: i64) -> i64 {
    if t > 0 { (now() - t).max(0) } else { -1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn friendly_units_with_precision() {
        assert_eq!(friendly_unit(1536, false, -1), "1.5\u{a0}KiB");
        assert_eq!(friendly_unit(1536, false, 3), "1.500\u{a0}KiB");
        assert_eq!(friendly_unit(1536, true, 0), "1\u{a0}KiB/s");
        assert_eq!(friendly_unit(10, false, 2), "10\u{a0}B");
        assert_eq!(friendly_unit(-1, false, 2), "Unknown");
    }
}
