//! Exact WebAPI view of preferences (`/api/v2/app/preferences` key order,
//! derived keys, units, partial `setPreferences` semantics). See
//! docs/spec/preferences.md, Appendix A/B, and qBittorrent's
//! `AppController::preferencesAction` / `setPreferencesAction`.
//!
//! `apply` mirrors qBittorrent's processing: keys are handled one by one in
//! qBittorrent's order, absent keys are left untouched, unknown keys are
//! ignored and values are coerced like `QVariant` does (`toBool`, `toInt`,
//! `toString`, `toReal`). Only the WebUI username/password can fail.

use serde_json::{Map, Value, json};

use super::*;
use crate::error::{QbtError, QbtResult};
use crate::model::{ContentLayout, ShareLimitAction, StopCondition};
use crate::webui::http::parse_int_str;
use crate::webui::subnet::{Subnet, parse_ip};

// ---- QVariant-like coercions ----------------------------------------------

/// `QVariant::toBool`.
fn to_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => {
            let t = s.trim();
            !(t.is_empty() || t == "0" || t.eq_ignore_ascii_case("false"))
        }
        _ => false,
    }
}

/// `QVariant::toLongLong` (malformed -> 0).
fn to_i64(v: &Value) -> i64 {
    match v {
        Value::Bool(b) => i64::from(*b),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.round() as i64))
            .unwrap_or(0),
        Value::String(s) => parse_int_str(s).unwrap_or(0),
        _ => 0,
    }
}

/// `QVariant::toInt` (32-bit; out of range or malformed -> 0).
fn to_int(v: &Value) -> i64 {
    let n = match v {
        Value::Number(n) => match n.as_i64() {
            Some(i) => i,
            None => n.as_f64().filter(|f| f.is_finite() && f.abs() < 1e18).map(|f| f.round() as i64).unwrap_or(0),
        },
        other => to_i64(other),
    };
    if i32::try_from(n).is_ok() { n } else { 0 }
}

/// `QVariant::toReal` (malformed or non-finite -> 0).
fn to_f64(v: &Value) -> f64 {
    let f = match v {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    };
    if f.is_finite() { f } else { 0.0 }
}

/// `QVariant::toString`.
fn to_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
        },
        _ => String::new(),
    }
}

/// Speed limits travel in B/s but are stored in KiB/s by qBittorrent: any
/// value from 1 to 1024 becomes 1 KiB/s; the WebAPI reports KiB x 1024.
fn normalize_speed(v: i64) -> i64 {
    if v <= 0 {
        0
    } else if v <= 1024 {
        1024
    } else {
        (v / 1024) * 1024
    }
}

fn port_in_range(v: i64) -> Option<i64> {
    (0..=65535).contains(&v).then_some(v)
}

// ---- enum <-> API names ------------------------------------------------------

fn content_layout_str(v: ContentLayout) -> &'static str {
    match v {
        ContentLayout::Original => "Original",
        ContentLayout::Subfolder => "Subfolder",
        ContentLayout::NoSubfolder => "NoSubfolder",
    }
}

fn content_layout_from(s: &str) -> ContentLayout {
    match s {
        "Subfolder" => ContentLayout::Subfolder,
        "NoSubfolder" => ContentLayout::NoSubfolder,
        _ => ContentLayout::Original,
    }
}

fn stop_condition_str(v: StopCondition) -> &'static str {
    match v {
        StopCondition::None => "None",
        StopCondition::MetadataReceived => "MetadataReceived",
        StopCondition::FilesChecked => "FilesChecked",
    }
}

fn stop_condition_from(s: &str) -> StopCondition {
    match s {
        "MetadataReceived" => StopCondition::MetadataReceived,
        "FilesChecked" => StopCondition::FilesChecked,
        _ => StopCondition::None,
    }
}

fn resume_storage_str(v: ResumeDataStorageType) -> &'static str {
    match v {
        ResumeDataStorageType::Legacy => "Legacy",
        ResumeDataStorageType::SQLite => "SQLite",
    }
}

fn content_remove_str(v: ContentRemoveOption) -> &'static str {
    match v {
        ContentRemoveOption::Delete => "Delete",
        ContentRemoveOption::MoveToTrash => "MoveToTrash",
    }
}

/// Global share-limit action: `Default` is never stored globally (= Stop).
fn global_share_action(v: ShareLimitAction) -> i32 {
    match v {
        ShareLimitAction::Default => ShareLimitAction::Stop.as_i32(),
        other => other.as_i32(),
    }
}

// ---- GET ---------------------------------------------------------------------

fn scan_dirs_json(folders: &[WatchedFolder]) -> Value {
    let mut m = Map::new();
    for f in folders {
        let v = if f.params.save_path.is_empty() {
            json!(1)
        } else if f.params.save_path == f.path {
            json!(0)
        } else {
            json!(f.params.save_path)
        };
        m.insert(f.path.clone(), v);
    }
    Value::Object(m)
}

/// Builds the `app/preferences` object (223 keys, Appendix A order).
pub fn to_json(p: &Preferences) -> Value {
    let b = &p.behavior;
    let d = &p.downloads;
    let c = &p.connection;
    let s = &p.speed;
    let bt = &p.bittorrent;
    let r = &p.rss;
    let w = &p.webui;
    let a = &p.advanced;
    let subnets: Vec<String> =
        w.bypass_auth_subnet_whitelist.iter().filter_map(|x| Subnet::parse(x)).map(|x| x.to_string()).collect();

    let mut m = Map::with_capacity(224);
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    // Behavior
    put("locale", json!(b.locale));
    put("performance_warning", json!(b.performance_warning));
    put("status_bar_external_ip", json!(b.status_bar_external_ip));
    put("confirm_torrent_deletion", json!(b.confirm_torrent_deletion));
    put("file_log_enabled", json!(b.file_log_enabled));
    put("file_log_path", json!(b.file_log_path));
    put("file_log_backup_enabled", json!(b.file_log_backup_enabled));
    put("file_log_max_size", json!(b.file_log_max_size));
    put("file_log_delete_old", json!(b.file_log_delete_old));
    put("file_log_age", json!(b.file_log_age));
    put("file_log_age_type", json!(b.file_log_age_type.as_i32()));
    put("delete_torrent_content_files", json!(b.delete_torrent_content_files));
    // Downloads
    put("torrent_content_layout", json!(content_layout_str(d.torrent_content_layout)));
    put("add_to_top_of_queue", json!(d.add_to_top_of_queue));
    put("add_stopped_enabled", json!(d.add_stopped_enabled));
    put("torrent_stop_condition", json!(stop_condition_str(d.torrent_stop_condition)));
    put("merge_trackers", json!(d.merge_trackers));
    put("auto_delete_mode", json!(d.auto_delete_mode.as_i32()));
    put("preallocate_all", json!(d.preallocate_all));
    put("incomplete_files_ext", json!(d.incomplete_files_ext));
    put("use_unwanted_folder", json!(d.use_unwanted_folder));
    put("auto_tmm_enabled", json!(d.auto_tmm_enabled));
    put("torrent_changed_tmm_enabled", json!(d.torrent_changed_tmm_enabled));
    put("save_path_changed_tmm_enabled", json!(d.save_path_changed_tmm_enabled));
    put("category_changed_tmm_enabled", json!(d.category_changed_tmm_enabled));
    put("save_path", json!(d.save_path));
    put("temp_path_enabled", json!(d.temp_path_enabled));
    put("temp_path", json!(d.temp_path));
    put("use_category_paths_in_manual_mode", json!(d.use_category_paths_in_manual_mode));
    put("export_dir", json!(d.export_dir));
    put("export_dir_fin", json!(d.export_dir_fin));
    put("scan_dirs", scan_dirs_json(&d.watched_folders));
    put("excluded_file_names_enabled", json!(d.excluded_file_names_enabled));
    put("excluded_file_names", json!(d.excluded_file_names.join("\n")));
    put("mail_notification_enabled", json!(d.mail_notification_enabled));
    put("mail_notification_sender", json!(d.mail_notification_sender));
    put("mail_notification_email", json!(d.mail_notification_email));
    put("mail_notification_smtp", json!(d.mail_notification_smtp));
    put("mail_notification_ssl_enabled", json!(d.mail_notification_ssl_enabled));
    put("mail_notification_auth_enabled", json!(d.mail_notification_auth_enabled));
    put("mail_notification_username", json!(d.mail_notification_username));
    put("mail_notification_password", json!(d.mail_notification_password));
    put("autorun_on_torrent_added_enabled", json!(d.autorun_on_torrent_added_enabled));
    put("autorun_on_torrent_added_program", json!(d.autorun_on_torrent_added_program));
    put("autorun_enabled", json!(d.autorun_enabled));
    put("autorun_program", json!(d.autorun_program));
    // Connection
    put("listen_port", json!(c.listen_port));
    put("ssl_enabled", json!(c.ssl_enabled));
    put("ssl_listen_port", json!(c.ssl_listen_port));
    put("random_port", json!(c.listen_port == 0));
    put("upnp", json!(c.upnp));
    put("max_connec", json!(c.max_connec));
    put("max_connec_per_torrent", json!(c.max_connec_per_torrent));
    put("max_uploads", json!(c.max_uploads));
    put("max_uploads_per_torrent", json!(c.max_uploads_per_torrent));
    put("i2p_enabled", json!(c.i2p_enabled));
    put("i2p_address", json!(c.i2p_address));
    put("i2p_port", json!(c.i2p_port));
    put("i2p_mixed_mode", json!(c.i2p_mixed_mode));
    put("i2p_inbound_quantity", json!(c.i2p_inbound_quantity));
    put("i2p_outbound_quantity", json!(c.i2p_outbound_quantity));
    put("i2p_inbound_length", json!(c.i2p_inbound_length));
    put("i2p_outbound_length", json!(c.i2p_outbound_length));
    put("proxy_type", json!(c.proxy_type.as_api_str()));
    put("proxy_ip", json!(c.proxy_ip));
    put("proxy_port", json!(c.proxy_port));
    put("proxy_auth_enabled", json!(c.proxy_auth_enabled));
    put("proxy_username", json!(c.proxy_username));
    put("proxy_password", json!(c.proxy_password));
    put("proxy_hostname_lookup", json!(c.proxy_hostname_lookup));
    put("proxy_bittorrent", json!(c.proxy_bittorrent));
    put("proxy_peer_connections", json!(c.proxy_peer_connections));
    put("proxy_rss", json!(c.proxy_rss));
    put("proxy_misc", json!(c.proxy_misc));
    put("ip_filter_enabled", json!(c.ip_filter_enabled));
    put("ip_filter_path", json!(c.ip_filter_path));
    put("ip_filter_trackers", json!(c.ip_filter_trackers));
    put("banned_IPs", json!(c.banned_ips.join("\n")));
    // Speed
    put("dl_limit", json!(normalize_speed(s.dl_limit)));
    put("up_limit", json!(normalize_speed(s.up_limit)));
    put("alt_dl_limit", json!(normalize_speed(s.alt_dl_limit)));
    put("alt_up_limit", json!(normalize_speed(s.alt_up_limit)));
    put("bittorrent_protocol", json!(c.bittorrent_protocol.as_i32()));
    put("limit_utp_rate", json!(s.limit_utp_rate));
    put("limit_tcp_overhead", json!(s.limit_tcp_overhead));
    put("limit_lan_peers", json!(s.limit_lan_peers));
    put("scheduler_enabled", json!(s.scheduler_enabled));
    put("schedule_from_hour", json!(s.schedule_from_hour));
    put("schedule_from_min", json!(s.schedule_from_min));
    put("schedule_to_hour", json!(s.schedule_to_hour));
    put("schedule_to_min", json!(s.schedule_to_min));
    put("scheduler_days", json!(s.scheduler_days.as_i32()));
    // BitTorrent
    put("dht", json!(bt.dht));
    put("pex", json!(bt.pex));
    put("lsd", json!(bt.lsd));
    put("encryption", json!(bt.encryption.as_i32()));
    put("anonymous_mode", json!(bt.anonymous_mode));
    put("max_active_checking_torrents", json!(bt.max_active_checking_torrents));
    put("queueing_enabled", json!(bt.queueing_enabled));
    put("max_active_downloads", json!(bt.max_active_downloads));
    put("max_active_torrents", json!(bt.max_active_torrents));
    put("max_active_uploads", json!(bt.max_active_uploads));
    put("dont_count_slow_torrents", json!(bt.dont_count_slow_torrents));
    put("slow_torrent_dl_rate_threshold", json!(bt.slow_torrent_dl_rate_threshold));
    put("slow_torrent_ul_rate_threshold", json!(bt.slow_torrent_ul_rate_threshold));
    put("slow_torrent_inactive_timer", json!(bt.slow_torrent_inactive_timer));
    put("max_ratio_enabled", json!(bt.max_ratio >= 0.0));
    put("max_ratio", json!(bt.max_ratio));
    put("max_seeding_time_enabled", json!(bt.max_seeding_time >= 0));
    put("max_seeding_time", json!(bt.max_seeding_time));
    put("max_inactive_seeding_time_enabled", json!(bt.max_inactive_seeding_time >= 0));
    put("max_inactive_seeding_time", json!(bt.max_inactive_seeding_time));
    put("max_ratio_act", json!(global_share_action(bt.max_ratio_act)));
    put("add_trackers_enabled", json!(bt.add_trackers_enabled));
    put("add_trackers", json!(bt.add_trackers));
    put("add_trackers_from_url_enabled", json!(bt.add_trackers_from_url_enabled));
    put("add_trackers_url", json!(bt.add_trackers_url));
    put("add_trackers_url_list", json!(bt.add_trackers_url_list));
    // WebUI
    put("web_ui_domain_list", json!(w.web_ui_domain_list));
    put("web_ui_address", json!(w.web_ui_address));
    put("web_ui_port", json!(w.web_ui_port));
    put("web_ui_upnp", json!(w.web_ui_upnp));
    put("use_https", json!(w.use_https));
    put("web_ui_https_cert_path", json!(w.web_ui_https_cert_path));
    put("web_ui_https_key_path", json!(w.web_ui_https_key_path));
    put("web_ui_username", json!(w.web_ui_username));
    put("bypass_local_auth", json!(w.bypass_local_auth));
    put("bypass_auth_subnet_whitelist_enabled", json!(w.bypass_auth_subnet_whitelist_enabled));
    put("bypass_auth_subnet_whitelist", json!(subnets.join("\n")));
    put("web_ui_max_auth_fail_count", json!(w.web_ui_max_auth_fail_count));
    put("web_ui_ban_duration", json!(w.web_ui_ban_duration));
    put("web_ui_session_timeout", json!(w.web_ui_session_timeout));
    put("web_ui_api_key", json!(w.web_ui_api_key));
    put("alternative_webui_enabled", json!(w.alternative_webui_enabled));
    put("alternative_webui_path", json!(w.alternative_webui_path));
    put("web_ui_clickjacking_protection_enabled", json!(w.web_ui_clickjacking_protection_enabled));
    put("web_ui_csrf_protection_enabled", json!(w.web_ui_csrf_protection_enabled));
    put("web_ui_secure_cookie_enabled", json!(w.web_ui_secure_cookie_enabled));
    put("web_ui_host_header_validation_enabled", json!(w.web_ui_host_header_validation_enabled));
    put("web_ui_use_custom_http_headers_enabled", json!(w.web_ui_use_custom_http_headers_enabled));
    put("web_ui_custom_http_headers", json!(w.web_ui_custom_http_headers));
    put("web_ui_reverse_proxy_enabled", json!(w.web_ui_reverse_proxy_enabled));
    put("web_ui_reverse_proxies_list", json!(w.web_ui_reverse_proxies_list));
    put("dyndns_enabled", json!(w.dyndns_enabled));
    put("dyndns_service", json!(w.dyndns_service.as_i32()));
    put("dyndns_username", json!(w.dyndns_username));
    put("dyndns_password", json!(w.dyndns_password));
    put("dyndns_domain", json!(w.dyndns_domain));
    // RSS
    put("rss_refresh_interval", json!(r.rss_refresh_interval));
    put("rss_fetch_delay", json!(r.rss_fetch_delay));
    put("rss_max_articles_per_feed", json!(r.rss_max_articles_per_feed));
    put("rss_processing_enabled", json!(r.rss_processing_enabled));
    put("rss_auto_downloading_enabled", json!(r.rss_auto_downloading_enabled));
    put("rss_download_repack_proper_episodes", json!(r.rss_download_repack_proper_episodes));
    put("rss_smart_episode_filters", json!(r.rss_smart_episode_filters.join("\n")));
    // Advanced: qBittorrent section
    put("resume_data_storage_type", json!(resume_storage_str(a.resume_data_storage_type)));
    put("torrent_content_remove_option", json!(content_remove_str(a.torrent_content_remove_option)));
    put("memory_working_set_limit", json!(a.memory_working_set_limit));
    put("current_network_interface", json!(a.current_network_interface));
    put("current_interface_name", json!(a.current_interface_name));
    put("current_interface_address", json!(a.current_interface_address));
    put("save_resume_data_interval", json!(a.save_resume_data_interval));
    put("save_statistics_interval", json!(a.save_statistics_interval));
    put("torrent_file_size_limit", json!(a.torrent_file_size_limit));
    put("confirm_torrent_recheck", json!(a.confirm_torrent_recheck));
    put("recheck_completed_torrents", json!(a.recheck_completed_torrents));
    put("app_instance_name", json!(a.app_instance_name));
    put("refresh_interval", json!(a.refresh_interval));
    put("resolve_peer_host_names", json!(a.resolve_peer_host_names));
    put("resolve_peer_countries", json!(a.resolve_peer_countries));
    put("reannounce_when_address_changed", json!(a.reannounce_when_address_changed));
    put("enable_embedded_tracker", json!(a.enable_embedded_tracker));
    put("embedded_tracker_port", json!(a.embedded_tracker_port));
    put("embedded_tracker_port_forwarding", json!(a.embedded_tracker_port_forwarding));
    put("mark_of_the_web", json!(a.mark_of_the_web));
    put("ignore_ssl_errors", json!(a.ignore_ssl_errors));
    put("python_executable_path", json!(a.python_executable_path));
    // Advanced: libtorrent section
    put("bdecode_depth_limit", json!(a.bdecode_depth_limit));
    put("bdecode_token_limit", json!(a.bdecode_token_limit));
    put("async_io_threads", json!(a.async_io_threads));
    put("hashing_threads", json!(a.hashing_threads.clamp(1, 1024)));
    put("file_pool_size", json!(a.file_pool_size));
    put("checking_memory_use", json!(a.checking_memory_use));
    put("disk_cache", json!(a.disk_cache.min(DISK_CACHE_MAX)));
    put("disk_cache_ttl", json!(a.disk_cache_ttl));
    put("disk_queue_size", json!(a.disk_queue_size));
    put("disk_io_type", json!(a.disk_io_type.as_i32()));
    put("disk_io_read_mode", json!(a.disk_io_read_mode.as_i32()));
    put("disk_io_write_mode", json!(a.disk_io_write_mode.as_i32()));
    put("enable_coalesce_read_write", json!(a.enable_coalesce_read_write));
    put("enable_piece_extent_affinity", json!(a.enable_piece_extent_affinity));
    put("enable_upload_suggestions", json!(a.enable_upload_suggestions));
    put("send_buffer_watermark", json!(a.send_buffer_watermark));
    put("send_buffer_low_watermark", json!(a.send_buffer_low_watermark));
    put("send_buffer_watermark_factor", json!(a.send_buffer_watermark_factor));
    put("connection_speed", json!(a.connection_speed));
    put("socket_send_buffer_size", json!(a.socket_send_buffer_size));
    put("socket_receive_buffer_size", json!(a.socket_receive_buffer_size));
    put("socket_backlog_size", json!(a.socket_backlog_size));
    put("outgoing_ports_min", json!(a.outgoing_ports_min));
    put("outgoing_ports_max", json!(a.outgoing_ports_max));
    put("upnp_lease_duration", json!(a.upnp_lease_duration));
    put("peer_tos", json!(a.peer_tos));
    put("utp_tcp_mixed_mode", json!(a.utp_tcp_mixed_mode.as_i32()));
    put("hostname_cache_ttl", json!(a.hostname_cache_ttl));
    put("idn_support_enabled", json!(a.idn_support_enabled));
    put("enable_multi_connections_from_same_ip", json!(a.enable_multi_connections_from_same_ip));
    put("validate_https_tracker_certificate", json!(a.validate_https_tracker_certificate));
    put("ssrf_mitigation", json!(a.ssrf_mitigation));
    put("block_peers_on_privileged_ports", json!(a.block_peers_on_privileged_ports));
    put("upload_slots_behavior", json!(a.upload_slots_behavior.as_i32()));
    put("upload_choking_algorithm", json!(a.upload_choking_algorithm.as_i32()));
    put("announce_to_all_trackers", json!(a.announce_to_all_trackers));
    put("announce_to_all_tiers", json!(a.announce_to_all_tiers));
    put("announce_ip", json!(a.announce_ip));
    put("announce_port", json!(a.announce_port));
    put("max_concurrent_http_announces", json!(a.max_concurrent_http_announces));
    put("stop_tracker_timeout", json!(a.stop_tracker_timeout));
    put("peer_turnover", json!(a.peer_turnover));
    put("peer_turnover_cutoff", json!(a.peer_turnover_cutoff));
    put("peer_turnover_interval", json!(a.peer_turnover_interval));
    put("request_queue_size", json!(a.request_queue_size));
    put("dht_bootstrap_nodes", json!(a.dht_bootstrap_nodes));
    Value::Object(m)
}

const DISK_CACHE_MAX: i64 = 33_554_431;

// ---- SET ---------------------------------------------------------------------

fn apply_scan_dirs(d: &mut DownloadsPrefs, v: &Value) {
    let Value::Object(dirs) = v else {
        // `toHash()` of a non-object is empty: every folder is removed.
        d.watched_folders.clear();
        return;
    };
    let old = std::mem::take(&mut d.watched_folders);
    let mut new = Vec::with_capacity(dirs.len());
    for (folder, val) in dirs {
        if folder.trim().is_empty() || new.iter().any(|f: &WatchedFolder| &f.path == folder) {
            continue;
        }
        let mut wf = old
            .iter()
            .find(|f| &f.path == folder)
            .cloned()
            .unwrap_or_else(|| WatchedFolder { path: folder.clone(), ..Default::default() });
        let as_int = match val {
            Value::Number(_) | Value::Bool(_) => Some(to_int(val)),
            Value::String(s) => parse_int_str(s),
            _ => None,
        };
        match as_int {
            Some(0) => {
                wf.params.save_path = folder.clone();
                wf.params.auto_tmm = Some(false);
            }
            Some(_) => {}
            None => {
                wf.params.save_path = to_str(val);
                wf.params.auto_tmm = Some(false);
            }
        }
        new.push(wf);
    }
    d.watched_folders = new;
}

fn apply_banned_ips(c: &mut ConnectionPrefs, text: &str) {
    let mut ips: Vec<String> = text.split('\n').filter_map(parse_ip).map(|ip| ip.to_string()).collect();
    ips.sort();
    ips.dedup();
    c.banned_ips = ips;
}

/// Applies a partial `setPreferences` object.
pub fn apply(p: &mut Preferences, obj: &Map<String, Value>) -> QbtResult<()> {
    let get = |k: &str| obj.get(k);
    macro_rules! set {
        ($key:literal, $field:expr, $conv:ident) => {
            if let Some(v) = get($key) {
                $field = $conv(v);
            }
        };
    }

    // ---- Behavior
    set!("locale", p.behavior.locale, to_str);
    set!("status_bar_external_ip", p.behavior.status_bar_external_ip, to_bool);
    set!("performance_warning", p.behavior.performance_warning, to_bool);
    set!("confirm_torrent_deletion", p.behavior.confirm_torrent_deletion, to_bool);
    set!("file_log_enabled", p.behavior.file_log_enabled, to_bool);
    set!("file_log_path", p.behavior.file_log_path, to_str);
    set!("file_log_backup_enabled", p.behavior.file_log_backup_enabled, to_bool);
    if let Some(v) = get("file_log_max_size") {
        p.behavior.file_log_max_size = to_int(v).clamp(1, 1_024_000);
    }
    set!("file_log_delete_old", p.behavior.file_log_delete_old, to_bool);
    if let Some(v) = get("file_log_age") {
        p.behavior.file_log_age = to_int(v).clamp(1, 365);
    }
    if let Some(v) = get("file_log_age_type") {
        p.behavior.file_log_age_type = FileLogAgeType::from_i32(to_int(v) as i32);
    }
    set!("delete_torrent_content_files", p.behavior.delete_torrent_content_files, to_bool);

    // ---- Downloads
    let d = &mut p.downloads;
    if let Some(v) = get("torrent_content_layout") {
        d.torrent_content_layout = content_layout_from(&to_str(v));
    }
    set!("add_to_top_of_queue", d.add_to_top_of_queue, to_bool);
    set!("add_stopped_enabled", d.add_stopped_enabled, to_bool);
    if let Some(v) = get("torrent_stop_condition") {
        d.torrent_stop_condition = stop_condition_from(&to_str(v));
    }
    set!("merge_trackers", d.merge_trackers, to_bool);
    if let Some(v) = get("auto_delete_mode") {
        d.auto_delete_mode = AutoDeleteMode::from_i32(to_int(v) as i32);
    }
    set!("preallocate_all", d.preallocate_all, to_bool);
    set!("incomplete_files_ext", d.incomplete_files_ext, to_bool);
    set!("use_unwanted_folder", d.use_unwanted_folder, to_bool);
    set!("auto_tmm_enabled", d.auto_tmm_enabled, to_bool);
    set!("torrent_changed_tmm_enabled", d.torrent_changed_tmm_enabled, to_bool);
    set!("save_path_changed_tmm_enabled", d.save_path_changed_tmm_enabled, to_bool);
    set!("category_changed_tmm_enabled", d.category_changed_tmm_enabled, to_bool);
    set!("save_path", d.save_path, to_str);
    set!("temp_path_enabled", d.temp_path_enabled, to_bool);
    set!("temp_path", d.temp_path, to_str);
    set!("use_category_paths_in_manual_mode", d.use_category_paths_in_manual_mode, to_bool);
    set!("export_dir", d.export_dir, to_str);
    set!("export_dir_fin", d.export_dir_fin, to_str);
    if let Some(v) = get("scan_dirs") {
        apply_scan_dirs(d, v);
    }
    set!("excluded_file_names_enabled", d.excluded_file_names_enabled, to_bool);
    if let Some(v) = get("excluded_file_names") {
        d.excluded_file_names = to_str(v).split('\n').map(str::to_owned).collect();
    }
    set!("mail_notification_enabled", d.mail_notification_enabled, to_bool);
    set!("mail_notification_sender", d.mail_notification_sender, to_str);
    set!("mail_notification_email", d.mail_notification_email, to_str);
    set!("mail_notification_smtp", d.mail_notification_smtp, to_str);
    set!("mail_notification_ssl_enabled", d.mail_notification_ssl_enabled, to_bool);
    set!("mail_notification_auth_enabled", d.mail_notification_auth_enabled, to_bool);
    set!("mail_notification_username", d.mail_notification_username, to_str);
    set!("mail_notification_password", d.mail_notification_password, to_str);
    set!("autorun_on_torrent_added_enabled", d.autorun_on_torrent_added_enabled, to_bool);
    if let Some(v) = get("autorun_on_torrent_added_program") {
        d.autorun_on_torrent_added_program = to_str(v).trim().to_owned();
    }
    set!("autorun_enabled", d.autorun_enabled, to_bool);
    if let Some(v) = get("autorun_program") {
        d.autorun_program = to_str(v).trim().to_owned();
    }

    // ---- Connection
    let c = &mut p.connection;
    if get("random_port").is_some_and(to_bool) {
        c.listen_port = 0;
    } else if let Some(v) = get("listen_port")
        && let Some(port) = port_in_range(to_int(v))
    {
        c.listen_port = port;
    }
    set!("ssl_enabled", c.ssl_enabled, to_bool);
    if let Some(v) = get("ssl_listen_port")
        && let Some(port) = port_in_range(to_int(v))
    {
        c.ssl_listen_port = port;
    }
    set!("upnp", c.upnp, to_bool);
    for (key, field) in [
        ("max_connec", &mut c.max_connec),
        ("max_connec_per_torrent", &mut c.max_connec_per_torrent),
        ("max_uploads", &mut c.max_uploads),
        ("max_uploads_per_torrent", &mut c.max_uploads_per_torrent),
    ] {
        if let Some(v) = get(key) {
            let n = to_int(v);
            *field = if n > 0 { n } else { -1 };
        }
    }
    set!("i2p_enabled", c.i2p_enabled, to_bool);
    set!("i2p_address", c.i2p_address, to_str);
    set!("i2p_port", c.i2p_port, to_int);
    set!("i2p_mixed_mode", c.i2p_mixed_mode, to_bool);
    set!("i2p_inbound_quantity", c.i2p_inbound_quantity, to_int);
    set!("i2p_outbound_quantity", c.i2p_outbound_quantity, to_int);
    set!("i2p_inbound_length", c.i2p_inbound_length, to_int);
    set!("i2p_outbound_length", c.i2p_outbound_length, to_int);
    if let Some(v) = get("proxy_type") {
        c.proxy_type = ProxyType::from_api(v);
    }
    set!("proxy_ip", c.proxy_ip, to_str);
    if let Some(v) = get("proxy_port")
        && let Some(port) = port_in_range(to_i64(v))
    {
        c.proxy_port = port;
    }
    set!("proxy_auth_enabled", c.proxy_auth_enabled, to_bool);
    set!("proxy_username", c.proxy_username, to_str);
    set!("proxy_password", c.proxy_password, to_str);
    set!("proxy_hostname_lookup", c.proxy_hostname_lookup, to_bool);
    set!("proxy_bittorrent", c.proxy_bittorrent, to_bool);
    set!("proxy_peer_connections", c.proxy_peer_connections, to_bool);
    set!("proxy_rss", c.proxy_rss, to_bool);
    set!("proxy_misc", c.proxy_misc, to_bool);
    set!("ip_filter_enabled", c.ip_filter_enabled, to_bool);
    set!("ip_filter_path", c.ip_filter_path, to_str);
    set!("ip_filter_trackers", c.ip_filter_trackers, to_bool);
    if let Some(v) = get("banned_IPs") {
        apply_banned_ips(c, &to_str(v));
    }

    // ---- Speed
    let s = &mut p.speed;
    for (key, field) in [
        ("dl_limit", &mut s.dl_limit),
        ("up_limit", &mut s.up_limit),
        ("alt_dl_limit", &mut s.alt_dl_limit),
        ("alt_up_limit", &mut s.alt_up_limit),
    ] {
        if let Some(v) = get(key) {
            *field = normalize_speed(to_int(v));
        }
    }
    if let Some(v) = get("bittorrent_protocol") {
        // Out-of-range values are ignored, like SessionImpl::setBTProtocol.
        let n = to_int(v);
        if (0..=2).contains(&n) {
            p.connection.bittorrent_protocol = BtProtocol::from_i32(n as i32);
        }
    }
    set!("limit_utp_rate", s.limit_utp_rate, to_bool);
    set!("limit_tcp_overhead", s.limit_tcp_overhead, to_bool);
    set!("limit_lan_peers", s.limit_lan_peers, to_bool);
    set!("scheduler_enabled", s.scheduler_enabled, to_bool);
    // Paired keys: applied only when both are present (and form a valid time).
    if let (Some(h), Some(m)) = (get("schedule_from_hour"), get("schedule_from_min")) {
        let (h, m) = (to_int(h), to_int(m));
        if (0..24).contains(&h) && (0..60).contains(&m) {
            s.schedule_from_hour = h;
            s.schedule_from_min = m;
        }
    }
    if let (Some(h), Some(m)) = (get("schedule_to_hour"), get("schedule_to_min")) {
        let (h, m) = (to_int(h), to_int(m));
        if (0..24).contains(&h) && (0..60).contains(&m) {
            s.schedule_to_hour = h;
            s.schedule_to_min = m;
        }
    }
    if let Some(v) = get("scheduler_days") {
        s.scheduler_days = SchedulerDays::from_i32(to_int(v) as i32);
    }

    // ---- BitTorrent
    let bt = &mut p.bittorrent;
    set!("dht", bt.dht, to_bool);
    set!("pex", bt.pex, to_bool);
    set!("lsd", bt.lsd, to_bool);
    if let Some(v) = get("encryption") {
        bt.encryption = EncryptionMode::from_i32(to_int(v) as i32);
    }
    set!("anonymous_mode", bt.anonymous_mode, to_bool);
    set!("max_active_checking_torrents", bt.max_active_checking_torrents, to_int);
    set!("queueing_enabled", bt.queueing_enabled, to_bool);
    set!("max_active_downloads", bt.max_active_downloads, to_int);
    set!("max_active_torrents", bt.max_active_torrents, to_int);
    set!("max_active_uploads", bt.max_active_uploads, to_int);
    set!("dont_count_slow_torrents", bt.dont_count_slow_torrents, to_bool);
    set!("slow_torrent_dl_rate_threshold", bt.slow_torrent_dl_rate_threshold, to_int);
    set!("slow_torrent_ul_rate_threshold", bt.slow_torrent_ul_rate_threshold, to_int);
    set!("slow_torrent_inactive_timer", bt.slow_torrent_inactive_timer, to_int);
    // Enable-flag + value pairs.
    if get("max_ratio_enabled").is_some_and(|v| !to_bool(v)) {
        bt.max_ratio = -1.0;
    } else if let Some(v) = get("max_ratio") {
        let r = to_f64(v);
        bt.max_ratio = if r < 0.0 { -1.0 } else { r };
    }
    if get("max_seeding_time_enabled").is_some_and(|v| !to_bool(v)) {
        bt.max_seeding_time = -1;
    } else if let Some(v) = get("max_seeding_time") {
        bt.max_seeding_time = to_int(v).max(-1);
    }
    if get("max_inactive_seeding_time_enabled").is_some_and(|v| !to_bool(v)) {
        bt.max_inactive_seeding_time = -1;
    } else if let Some(v) = get("max_inactive_seeding_time") {
        bt.max_inactive_seeding_time = to_int(v).max(-1);
    }
    if let Some(v) = get("max_ratio_act") {
        bt.max_ratio_act = match to_int(v) {
            1 => ShareLimitAction::Remove,
            2 => ShareLimitAction::EnableSuperSeeding,
            3 => ShareLimitAction::RemoveWithContent,
            _ => ShareLimitAction::Stop,
        };
    }
    set!("add_trackers_enabled", bt.add_trackers_enabled, to_bool);
    set!("add_trackers", bt.add_trackers, to_str);
    set!("add_trackers_from_url_enabled", bt.add_trackers_from_url_enabled, to_bool);
    if let Some(v) = get("add_trackers_url") {
        bt.add_trackers_url = to_str(v).trim().to_owned();
    }

    // ---- WebUI
    let w = &mut p.webui;
    set!("web_ui_domain_list", w.web_ui_domain_list, to_str);
    if let Some(v) = get("web_ui_address") {
        w.web_ui_address = to_str(v).trim().to_owned();
    }
    if let Some(v) = get("web_ui_port") {
        let port = to_int(v);
        if (1..=65535).contains(&port) {
            w.web_ui_port = port;
        }
    }
    set!("web_ui_upnp", w.web_ui_upnp, to_bool);
    set!("use_https", w.use_https, to_bool);
    set!("web_ui_https_cert_path", w.web_ui_https_cert_path, to_str);
    set!("web_ui_https_key_path", w.web_ui_https_key_path, to_str);
    if let Some(v) = get("web_ui_username") {
        let username = to_str(v);
        if username.encode_utf16().count() < 3 {
            return Err(QbtError::invalid("WebUI username must be at least 3 characters long"));
        }
        if username.contains(':') {
            return Err(QbtError::invalid("WebUI username cannot contain a colon"));
        }
        w.web_ui_username = username;
    }
    if let Some(v) = get("web_ui_password") {
        let password = to_str(v);
        if password.encode_utf16().count() < 6 {
            return Err(QbtError::invalid("WebUI password must be at least 6 characters long"));
        }
        w.web_ui_password_pbkdf2 = crate::webui::password::hash_password(&password);
    }
    set!("bypass_local_auth", w.bypass_local_auth, to_bool);
    set!("bypass_auth_subnet_whitelist_enabled", w.bypass_auth_subnet_whitelist_enabled, to_bool);
    if let Some(v) = get("bypass_auth_subnet_whitelist") {
        w.bypass_auth_subnet_whitelist = to_str(v)
            .split(['\n', ','])
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_owned)
            .collect();
    }
    set!("web_ui_max_auth_fail_count", w.web_ui_max_auth_fail_count, to_int);
    set!("web_ui_ban_duration", w.web_ui_ban_duration, to_int);
    set!("web_ui_session_timeout", w.web_ui_session_timeout, to_int);
    set!("alternative_webui_enabled", w.alternative_webui_enabled, to_bool);
    set!("alternative_webui_path", w.alternative_webui_path, to_str);
    set!("web_ui_clickjacking_protection_enabled", w.web_ui_clickjacking_protection_enabled, to_bool);
    set!("web_ui_csrf_protection_enabled", w.web_ui_csrf_protection_enabled, to_bool);
    set!("web_ui_secure_cookie_enabled", w.web_ui_secure_cookie_enabled, to_bool);
    set!("web_ui_host_header_validation_enabled", w.web_ui_host_header_validation_enabled, to_bool);
    set!("web_ui_use_custom_http_headers_enabled", w.web_ui_use_custom_http_headers_enabled, to_bool);
    set!("web_ui_custom_http_headers", w.web_ui_custom_http_headers, to_str);
    set!("web_ui_reverse_proxy_enabled", w.web_ui_reverse_proxy_enabled, to_bool);
    set!("web_ui_reverse_proxies_list", w.web_ui_reverse_proxies_list, to_str);
    set!("dyndns_enabled", w.dyndns_enabled, to_bool);
    if let Some(v) = get("dyndns_service") {
        w.dyndns_service = DynDnsService::from_i32(to_int(v) as i32);
    }
    set!("dyndns_username", w.dyndns_username, to_str);
    set!("dyndns_password", w.dyndns_password, to_str);
    set!("dyndns_domain", w.dyndns_domain, to_str);

    // ---- RSS
    let r = &mut p.rss;
    set!("rss_refresh_interval", r.rss_refresh_interval, to_int);
    set!("rss_fetch_delay", r.rss_fetch_delay, to_i64);
    set!("rss_max_articles_per_feed", r.rss_max_articles_per_feed, to_int);
    set!("rss_processing_enabled", r.rss_processing_enabled, to_bool);
    set!("rss_auto_downloading_enabled", r.rss_auto_downloading_enabled, to_bool);
    set!("rss_download_repack_proper_episodes", r.rss_download_repack_proper_episodes, to_bool);
    if let Some(v) = get("rss_smart_episode_filters") {
        r.rss_smart_episode_filters = to_str(v).split('\n').map(str::to_owned).collect();
    }

    // ---- Advanced
    let a = &mut p.advanced;
    if let Some(v) = get("resume_data_storage_type") {
        a.resume_data_storage_type = match to_str(v).as_str() {
            "SQLite" => ResumeDataStorageType::SQLite,
            _ => ResumeDataStorageType::Legacy,
        };
    }
    if let Some(v) = get("torrent_content_remove_option") {
        // Unparsable values fall back to MoveToTrash (qBittorrent's choice).
        a.torrent_content_remove_option = match to_str(v).as_str() {
            "Delete" => ContentRemoveOption::Delete,
            _ => ContentRemoveOption::MoveToTrash,
        };
    }
    set!("memory_working_set_limit", a.memory_working_set_limit, to_int);
    if let Some(v) = get("current_network_interface") {
        let value = to_str(v);
        let name = crate::net::interfaces::list()
            .into_iter()
            .find(|i| !i.addresses.is_empty() && i.value == value)
            .map(|i| i.name)
            .unwrap_or_default();
        if !name.is_empty() || value.is_empty() {
            a.current_interface_name = name;
        }
        a.current_network_interface = value;
    }
    if let Some(v) = get("current_interface_address") {
        a.current_interface_address = parse_ip(&to_str(v)).map(|ip| ip.to_string()).unwrap_or_default();
    }
    set!("save_resume_data_interval", a.save_resume_data_interval, to_int);
    set!("save_statistics_interval", a.save_statistics_interval, to_int);
    set!("torrent_file_size_limit", a.torrent_file_size_limit, to_i64);
    set!("confirm_torrent_recheck", a.confirm_torrent_recheck, to_bool);
    set!("recheck_completed_torrents", a.recheck_completed_torrents, to_bool);
    set!("app_instance_name", a.app_instance_name, to_str);
    set!("refresh_interval", a.refresh_interval, to_int);
    set!("resolve_peer_host_names", a.resolve_peer_host_names, to_bool);
    set!("resolve_peer_countries", a.resolve_peer_countries, to_bool);
    set!("reannounce_when_address_changed", a.reannounce_when_address_changed, to_bool);
    set!("embedded_tracker_port", a.embedded_tracker_port, to_int);
    set!("embedded_tracker_port_forwarding", a.embedded_tracker_port_forwarding, to_bool);
    set!("enable_embedded_tracker", a.enable_embedded_tracker, to_bool);
    set!("mark_of_the_web", a.mark_of_the_web, to_bool);
    set!("ignore_ssl_errors", a.ignore_ssl_errors, to_bool);
    set!("python_executable_path", a.python_executable_path, to_str);
    set!("bdecode_depth_limit", a.bdecode_depth_limit, to_int);
    set!("bdecode_token_limit", a.bdecode_token_limit, to_int);
    set!("async_io_threads", a.async_io_threads, to_int);
    if let Some(v) = get("hashing_threads") {
        a.hashing_threads = to_int(v).clamp(1, 1024);
    }
    set!("file_pool_size", a.file_pool_size, to_int);
    if let Some(v) = get("checking_memory_use") {
        a.checking_memory_use = to_int(v).max(1);
    }
    if let Some(v) = get("disk_cache") {
        a.disk_cache = to_int(v).min(DISK_CACHE_MAX);
    }
    set!("disk_cache_ttl", a.disk_cache_ttl, to_int);
    set!("disk_queue_size", a.disk_queue_size, to_i64);
    if let Some(v) = get("disk_io_type") {
        a.disk_io_type = DiskIoType::from_i32(to_int(v) as i32);
    }
    if let Some(v) = get("disk_io_read_mode") {
        a.disk_io_read_mode = DiskIoReadMode::from_i32(to_int(v) as i32);
    }
    if let Some(v) = get("disk_io_write_mode") {
        a.disk_io_write_mode = DiskIoWriteMode::from_i32(to_int(v) as i32);
    }
    set!("enable_coalesce_read_write", a.enable_coalesce_read_write, to_bool);
    set!("enable_piece_extent_affinity", a.enable_piece_extent_affinity, to_bool);
    set!("enable_upload_suggestions", a.enable_upload_suggestions, to_bool);
    set!("send_buffer_watermark", a.send_buffer_watermark, to_int);
    set!("send_buffer_low_watermark", a.send_buffer_low_watermark, to_int);
    set!("send_buffer_watermark_factor", a.send_buffer_watermark_factor, to_int);
    set!("connection_speed", a.connection_speed, to_int);
    set!("socket_send_buffer_size", a.socket_send_buffer_size, to_int);
    set!("socket_receive_buffer_size", a.socket_receive_buffer_size, to_int);
    set!("socket_backlog_size", a.socket_backlog_size, to_int);
    set!("outgoing_ports_min", a.outgoing_ports_min, to_int);
    set!("outgoing_ports_max", a.outgoing_ports_max, to_int);
    set!("upnp_lease_duration", a.upnp_lease_duration, to_int);
    set!("peer_tos", a.peer_tos, to_int);
    if let Some(v) = get("utp_tcp_mixed_mode") {
        a.utp_tcp_mixed_mode = UtpMixedMode::from_i32(to_int(v) as i32);
    }
    set!("hostname_cache_ttl", a.hostname_cache_ttl, to_int);
    set!("idn_support_enabled", a.idn_support_enabled, to_bool);
    set!("enable_multi_connections_from_same_ip", a.enable_multi_connections_from_same_ip, to_bool);
    set!("validate_https_tracker_certificate", a.validate_https_tracker_certificate, to_bool);
    set!("ssrf_mitigation", a.ssrf_mitigation, to_bool);
    set!("block_peers_on_privileged_ports", a.block_peers_on_privileged_ports, to_bool);
    if let Some(v) = get("upload_slots_behavior") {
        a.upload_slots_behavior = UploadSlotsBehavior::from_i32(to_int(v) as i32);
    }
    if let Some(v) = get("upload_choking_algorithm") {
        a.upload_choking_algorithm = UploadChokingAlgorithm::from_i32(to_int(v) as i32);
    }
    set!("announce_to_all_trackers", a.announce_to_all_trackers, to_bool);
    set!("announce_to_all_tiers", a.announce_to_all_tiers, to_bool);
    if let Some(v) = get("announce_ip") {
        a.announce_ip = parse_ip(&to_str(v)).map(|ip| ip.to_string()).unwrap_or_default();
    }
    set!("announce_port", a.announce_port, to_int);
    set!("max_concurrent_http_announces", a.max_concurrent_http_announces, to_int);
    set!("stop_tracker_timeout", a.stop_tracker_timeout, to_int);
    set!("peer_turnover", a.peer_turnover, to_int);
    set!("peer_turnover_cutoff", a.peer_turnover_cutoff, to_int);
    set!("peer_turnover_interval", a.peer_turnover_interval, to_int);
    set!("request_queue_size", a.request_queue_size, to_int);
    if let Some(v) = get("dht_bootstrap_nodes") {
        let nodes = to_str(v);
        a.dht_bootstrap_nodes =
            if nodes.trim().is_empty() { DEFAULT_DHT_BOOTSTRAP_NODES.to_owned() } else { nodes };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Appendix A of docs/spec/preferences.md, in order.
    const APPENDIX_A: &[&str] = &[
        "locale", "performance_warning", "status_bar_external_ip", "confirm_torrent_deletion", "file_log_enabled",
        "file_log_path", "file_log_backup_enabled", "file_log_max_size", "file_log_delete_old", "file_log_age",
        "file_log_age_type", "delete_torrent_content_files", "torrent_content_layout", "add_to_top_of_queue",
        "add_stopped_enabled", "torrent_stop_condition", "merge_trackers", "auto_delete_mode", "preallocate_all",
        "incomplete_files_ext", "use_unwanted_folder", "auto_tmm_enabled", "torrent_changed_tmm_enabled",
        "save_path_changed_tmm_enabled", "category_changed_tmm_enabled", "save_path", "temp_path_enabled", "temp_path",
        "use_category_paths_in_manual_mode", "export_dir", "export_dir_fin", "scan_dirs", "excluded_file_names_enabled",
        "excluded_file_names", "mail_notification_enabled", "mail_notification_sender", "mail_notification_email",
        "mail_notification_smtp", "mail_notification_ssl_enabled", "mail_notification_auth_enabled",
        "mail_notification_username", "mail_notification_password", "autorun_on_torrent_added_enabled",
        "autorun_on_torrent_added_program", "autorun_enabled", "autorun_program", "listen_port", "ssl_enabled",
        "ssl_listen_port", "random_port", "upnp", "max_connec", "max_connec_per_torrent", "max_uploads",
        "max_uploads_per_torrent", "i2p_enabled", "i2p_address", "i2p_port", "i2p_mixed_mode", "i2p_inbound_quantity",
        "i2p_outbound_quantity", "i2p_inbound_length", "i2p_outbound_length", "proxy_type", "proxy_ip", "proxy_port",
        "proxy_auth_enabled", "proxy_username", "proxy_password", "proxy_hostname_lookup", "proxy_bittorrent",
        "proxy_peer_connections", "proxy_rss", "proxy_misc", "ip_filter_enabled", "ip_filter_path",
        "ip_filter_trackers", "banned_IPs", "dl_limit", "up_limit", "alt_dl_limit", "alt_up_limit",
        "bittorrent_protocol", "limit_utp_rate", "limit_tcp_overhead", "limit_lan_peers", "scheduler_enabled",
        "schedule_from_hour", "schedule_from_min", "schedule_to_hour", "schedule_to_min", "scheduler_days", "dht",
        "pex", "lsd", "encryption", "anonymous_mode", "max_active_checking_torrents", "queueing_enabled",
        "max_active_downloads", "max_active_torrents", "max_active_uploads", "dont_count_slow_torrents",
        "slow_torrent_dl_rate_threshold", "slow_torrent_ul_rate_threshold", "slow_torrent_inactive_timer",
        "max_ratio_enabled", "max_ratio", "max_seeding_time_enabled", "max_seeding_time",
        "max_inactive_seeding_time_enabled", "max_inactive_seeding_time", "max_ratio_act", "add_trackers_enabled",
        "add_trackers", "add_trackers_from_url_enabled", "add_trackers_url", "add_trackers_url_list",
        "web_ui_domain_list", "web_ui_address", "web_ui_port", "web_ui_upnp", "use_https", "web_ui_https_cert_path",
        "web_ui_https_key_path", "web_ui_username", "bypass_local_auth", "bypass_auth_subnet_whitelist_enabled",
        "bypass_auth_subnet_whitelist", "web_ui_max_auth_fail_count", "web_ui_ban_duration", "web_ui_session_timeout",
        "web_ui_api_key", "alternative_webui_enabled", "alternative_webui_path",
        "web_ui_clickjacking_protection_enabled", "web_ui_csrf_protection_enabled", "web_ui_secure_cookie_enabled",
        "web_ui_host_header_validation_enabled", "web_ui_use_custom_http_headers_enabled", "web_ui_custom_http_headers",
        "web_ui_reverse_proxy_enabled", "web_ui_reverse_proxies_list", "dyndns_enabled", "dyndns_service",
        "dyndns_username", "dyndns_password", "dyndns_domain", "rss_refresh_interval", "rss_fetch_delay",
        "rss_max_articles_per_feed", "rss_processing_enabled", "rss_auto_downloading_enabled",
        "rss_download_repack_proper_episodes", "rss_smart_episode_filters", "resume_data_storage_type",
        "torrent_content_remove_option", "memory_working_set_limit", "current_network_interface",
        "current_interface_name", "current_interface_address", "save_resume_data_interval", "save_statistics_interval",
        "torrent_file_size_limit", "confirm_torrent_recheck", "recheck_completed_torrents", "app_instance_name",
        "refresh_interval", "resolve_peer_host_names", "resolve_peer_countries", "reannounce_when_address_changed",
        "enable_embedded_tracker", "embedded_tracker_port", "embedded_tracker_port_forwarding", "mark_of_the_web",
        "ignore_ssl_errors", "python_executable_path", "bdecode_depth_limit", "bdecode_token_limit",
        "async_io_threads", "hashing_threads", "file_pool_size", "checking_memory_use", "disk_cache", "disk_cache_ttl",
        "disk_queue_size", "disk_io_type", "disk_io_read_mode", "disk_io_write_mode", "enable_coalesce_read_write",
        "enable_piece_extent_affinity", "enable_upload_suggestions", "send_buffer_watermark",
        "send_buffer_low_watermark", "send_buffer_watermark_factor", "connection_speed", "socket_send_buffer_size",
        "socket_receive_buffer_size", "socket_backlog_size", "outgoing_ports_min", "outgoing_ports_max",
        "upnp_lease_duration", "peer_tos", "utp_tcp_mixed_mode", "hostname_cache_ttl", "idn_support_enabled",
        "enable_multi_connections_from_same_ip", "validate_https_tracker_certificate", "ssrf_mitigation",
        "block_peers_on_privileged_ports", "upload_slots_behavior", "upload_choking_algorithm",
        "announce_to_all_trackers", "announce_to_all_tiers", "announce_ip", "announce_port",
        "max_concurrent_http_announces", "stop_tracker_timeout", "peer_turnover", "peer_turnover_cutoff",
        "peer_turnover_interval", "request_queue_size", "dht_bootstrap_nodes",
    ];

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn get_has_exact_key_order() {
        assert_eq!(APPENDIX_A.len(), 223);
        let v = to_json(&Preferences::default());
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, APPENDIX_A);
        // Serialized text keeps the order too.
        let text = v.to_string();
        assert!(text.starts_with("{\"locale\":"));
        assert!(text.ends_with(&format!("\"dht_bootstrap_nodes\":\"{}\"}}", DEFAULT_DHT_BOOTSTRAP_NODES)));
    }

    #[test]
    fn get_defaults_and_derived_keys() {
        let v = to_json(&Preferences::default());
        assert_eq!(v["file_log_max_size"], 65);
        assert_eq!(v["torrent_content_layout"], "Original");
        assert_eq!(v["torrent_stop_condition"], "None");
        assert_eq!(v["proxy_type"], "None");
        assert_eq!(v["resume_data_storage_type"], "Legacy");
        assert_eq!(v["torrent_content_remove_option"], "Delete");
        assert_eq!(v["max_ratio_enabled"], false);
        assert_eq!(v["max_seeding_time_enabled"], false);
        assert_eq!(v["max_ratio_act"], 0);
        assert_eq!(v["random_port"], false);
        assert_eq!(v["alt_dl_limit"], 10240);
        assert_eq!(v["scan_dirs"], json!({}));
        assert_eq!(v["banned_IPs"], "");
        assert_eq!(v["file_log_age_type"], 1);
        assert_eq!(v["rss_smart_episode_filters"].as_str().unwrap().lines().count(), 4);
        assert!(v.get("web_ui_password").is_none());
        assert!(v.get("web_ui_password_pbkdf2").is_none());
    }

    #[test]
    fn round_trip_full_object_is_identity() {
        let mut p = Preferences::default();
        p.downloads.save_path = "/data".into();
        p.connection.listen_port = 6881;
        p.connection.banned_ips = vec!["1.2.3.4".into(), "::1".into()];
        p.speed.dl_limit = 2048;
        p.bittorrent.max_ratio = 1.5;
        p.downloads.watched_folders = vec![
            WatchedFolder { path: "/w/a".into(), ..Default::default() },
            WatchedFolder {
                path: "/w/b".into(),
                params: crate::model::TorrentParamsTemplate {
                    save_path: "/w/b".into(),
                    auto_tmm: Some(false),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];
        p.webui.bypass_auth_subnet_whitelist = vec!["10.0.0.0/8".into()];
        let v = to_json(&p);
        let mut q = p.clone();
        apply(&mut q, v.as_object().unwrap()).unwrap();
        assert_eq!(to_json(&q), v);
        assert_eq!(q.downloads.watched_folders.len(), 2);
    }

    #[test]
    fn partial_update_leaves_other_keys() {
        let mut p = Preferences::default();
        apply(&mut p, &obj(json!({"dht": false, "unknown_key": 5}))).unwrap();
        let mut expected = Preferences::default();
        expected.bittorrent.dht = false;
        assert_eq!(p, expected);
    }

    #[test]
    fn qvariant_coercions() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "dht": "false", "pex": 0, "lsd": "yes", "upnp": "0", "anonymous_mode": 2.5,
                "max_active_downloads": "7", "max_active_torrents": 9.6, "max_active_uploads": "x",
                "save_path": 12, "export_dir": true, "max_ratio": "2.25", "rss_fetch_delay": "  9 ",
            })),
        )
        .unwrap();
        assert!(!p.bittorrent.dht);
        assert!(!p.bittorrent.pex);
        assert!(p.bittorrent.lsd);
        assert!(!p.connection.upnp);
        assert!(p.bittorrent.anonymous_mode);
        assert_eq!(p.bittorrent.max_active_downloads, 7);
        assert_eq!(p.bittorrent.max_active_torrents, 10);
        assert_eq!(p.bittorrent.max_active_uploads, 0); // malformed -> 0
        assert_eq!(p.downloads.save_path, "12");
        assert_eq!(p.downloads.export_dir, "true");
        assert_eq!(p.bittorrent.max_ratio, 2.25);
        assert_eq!(p.rss.rss_fetch_delay, 9);
    }

    #[test]
    fn paired_schedule_keys() {
        let mut p = Preferences::default();
        apply(&mut p, &obj(json!({"schedule_from_hour": 3, "schedule_to_min": 15}))).unwrap();
        assert_eq!((p.speed.schedule_from_hour, p.speed.schedule_to_min), (8, 0));
        apply(&mut p, &obj(json!({"schedule_from_hour": 3, "schedule_from_min": 30}))).unwrap();
        assert_eq!((p.speed.schedule_from_hour, p.speed.schedule_from_min), (3, 30));
        apply(&mut p, &obj(json!({"schedule_to_hour": 25, "schedule_to_min": 0}))).unwrap();
        assert_eq!(p.speed.schedule_to_hour, 20);
    }

    #[test]
    fn enable_flag_pairs() {
        let mut p = Preferences::default();
        apply(&mut p, &obj(json!({"max_ratio": 2.0, "max_seeding_time": 60}))).unwrap();
        assert_eq!(p.bittorrent.max_ratio, 2.0);
        assert_eq!(p.bittorrent.max_seeding_time, 60);
        apply(&mut p, &obj(json!({"max_ratio_enabled": false, "max_ratio": 5.0}))).unwrap();
        assert_eq!(p.bittorrent.max_ratio, -1.0);
        apply(&mut p, &obj(json!({"max_seeding_time_enabled": true, "max_seeding_time": 30}))).unwrap();
        assert_eq!(p.bittorrent.max_seeding_time, 30);
        apply(&mut p, &obj(json!({"max_inactive_seeding_time_enabled": false}))).unwrap();
        assert_eq!(p.bittorrent.max_inactive_seeding_time, -1);
        apply(&mut p, &obj(json!({"max_ratio": -7, "max_seeding_time": -9}))).unwrap();
        assert_eq!(p.bittorrent.max_ratio, -1.0);
        assert_eq!(p.bittorrent.max_seeding_time, -1);
        let v = to_json(&p);
        assert_eq!(v["max_ratio_enabled"], false);
    }

    #[test]
    fn random_port_and_listen_port() {
        let mut p = Preferences::default();
        apply(&mut p, &obj(json!({"listen_port": 5000}))).unwrap();
        assert_eq!(p.connection.listen_port, 5000);
        apply(&mut p, &obj(json!({"random_port": true, "listen_port": 7000}))).unwrap();
        assert_eq!(p.connection.listen_port, 0);
        assert_eq!(to_json(&p)["random_port"], true);
        apply(&mut p, &obj(json!({"random_port": false, "listen_port": 7000}))).unwrap();
        assert_eq!(p.connection.listen_port, 7000);
        apply(&mut p, &obj(json!({"listen_port": 70000}))).unwrap();
        assert_eq!(p.connection.listen_port, 7000);
    }

    #[test]
    fn string_enums_and_fallbacks() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "torrent_content_layout": "Subfolder", "torrent_stop_condition": "FilesChecked",
                "proxy_type": "SOCKS5", "resume_data_storage_type": "SQLite",
                "torrent_content_remove_option": "Delete",
            })),
        )
        .unwrap();
        assert_eq!(p.downloads.torrent_content_layout, ContentLayout::Subfolder);
        assert_eq!(p.downloads.torrent_stop_condition, StopCondition::FilesChecked);
        assert_eq!(p.connection.proxy_type, ProxyType::Socks5);
        assert_eq!(p.advanced.resume_data_storage_type, ResumeDataStorageType::SQLite);
        apply(
            &mut p,
            &obj(json!({
                "torrent_content_layout": "bogus", "torrent_stop_condition": 3, "proxy_type": "x",
                "resume_data_storage_type": "sqlite", "torrent_content_remove_option": "nope",
            })),
        )
        .unwrap();
        assert_eq!(p.downloads.torrent_content_layout, ContentLayout::Original);
        assert_eq!(p.downloads.torrent_stop_condition, StopCondition::None);
        assert_eq!(p.connection.proxy_type, ProxyType::None);
        assert_eq!(p.advanced.resume_data_storage_type, ResumeDataStorageType::Legacy);
        // Special case: unparsable removal option means MoveToTrash.
        assert_eq!(p.advanced.torrent_content_remove_option, ContentRemoveOption::MoveToTrash);
        // Integer enums: out of range -> default; bittorrent_protocol keeps the old value.
        apply(&mut p, &obj(json!({"bittorrent_protocol": 1}))).unwrap();
        apply(&mut p, &obj(json!({"bittorrent_protocol": 9, "file_log_age_type": 7, "max_ratio_act": 9}))).unwrap();
        assert_eq!(p.connection.bittorrent_protocol, BtProtocol::Tcp);
        assert_eq!(p.behavior.file_log_age_type, FileLogAgeType::Months);
        assert_eq!(p.bittorrent.max_ratio_act, ShareLimitAction::Stop);
        apply(&mut p, &obj(json!({"max_ratio_act": 3}))).unwrap();
        assert_eq!(to_json(&p)["max_ratio_act"], 3);
    }

    #[test]
    fn scan_dirs_semantics() {
        let mut p = Preferences::default();
        p.downloads.watched_folders = vec![WatchedFolder {
            path: "/keep".into(),
            recursive: true,
            params: crate::model::TorrentParamsTemplate { category: "tv".into(), ..Default::default() },
        }];
        apply(&mut p, &obj(json!({"scan_dirs": {"/keep": 1, "/zero": 0, "/custom": "/dl", "": 1}}))).unwrap();
        let f = &p.downloads.watched_folders;
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].path, "/keep");
        assert!(f[0].recursive);
        assert_eq!(f[0].params.category, "tv");
        assert_eq!(f[0].params.save_path, "");
        assert_eq!(f[1].params.save_path, "/zero");
        assert_eq!(f[1].params.auto_tmm, Some(false));
        assert_eq!(f[2].params.save_path, "/dl");
        // Folders added through the API inherit the global limits.
        assert_eq!((f[1].params.upload_limit, f[1].params.download_limit), (-1, -1));
        assert_eq!(f[1].params.ratio_limit, -2.0);
        assert_eq!((f[1].params.seeding_time_limit, f[1].params.inactive_seeding_time_limit), (-2, -2));
        assert_eq!(to_json(&p)["scan_dirs"], json!({"/keep": 1, "/zero": 0, "/custom": "/dl"}));
        // Absent folders are removed.
        apply(&mut p, &obj(json!({"scan_dirs": {"/zero": 0}}))).unwrap();
        assert_eq!(p.downloads.watched_folders.len(), 1);
    }

    #[test]
    fn list_keys() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "banned_IPs": "10.0.0.2\n\n bad \n10.0.0.1\n::FFFF:10.0.0.1\n2001:db8::1",
                "bypass_auth_subnet_whitelist": "192.168.1.0/24, 10.0.0.0/8\n\nnot-a-subnet",
                "excluded_file_names": "*.exe\n\nreadme.txt",
                "rss_smart_episode_filters": "a\n\nb",
            })),
        )
        .unwrap();
        assert_eq!(p.connection.banned_ips, vec!["10.0.0.1", "10.0.0.2", "2001:db8::1"]);
        assert_eq!(p.webui.bypass_auth_subnet_whitelist, vec!["192.168.1.0/24", "10.0.0.0/8", "not-a-subnet"]);
        assert_eq!(p.downloads.excluded_file_names, vec!["*.exe", "", "readme.txt"]);
        assert_eq!(p.rss.rss_smart_episode_filters, vec!["a", "", "b"]);
        // Unparsable subnets are dropped when read.
        assert_eq!(to_json(&p)["bypass_auth_subnet_whitelist"], "192.168.1.0/24\n10.0.0.0/8");
    }

    #[test]
    fn units_and_clamps() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "dl_limit": 500, "up_limit": 3000, "alt_dl_limit": -5, "alt_up_limit": 1024,
                "file_log_max_size": 0, "file_log_age": 999, "max_connec": 0, "max_uploads": -3,
                "max_connec_per_torrent": 50, "hashing_threads": 0, "disk_cache": 99999999,
                "checking_memory_use": -1, "web_ui_port": 0, "dht_bootstrap_nodes": "  ",
            })),
        )
        .unwrap();
        assert_eq!(p.speed.dl_limit, 1024);
        assert_eq!(p.speed.up_limit, 2048);
        assert_eq!(p.speed.alt_dl_limit, 0);
        assert_eq!(p.speed.alt_up_limit, 1024);
        assert_eq!(p.behavior.file_log_max_size, 1);
        assert_eq!(p.behavior.file_log_age, 365);
        assert_eq!(p.connection.max_connec, -1);
        assert_eq!(p.connection.max_uploads, -1);
        assert_eq!(p.connection.max_connec_per_torrent, 50);
        assert_eq!(p.advanced.hashing_threads, 1);
        assert_eq!(p.advanced.disk_cache, DISK_CACHE_MAX);
        assert_eq!(p.advanced.checking_memory_use, 1);
        assert_eq!(p.webui.web_ui_port, 8080);
        assert_eq!(p.advanced.dht_bootstrap_nodes, DEFAULT_DHT_BOOTSTRAP_NODES);
        assert_eq!(to_json(&p)["up_limit"], 2048);
    }

    #[test]
    fn inverted_and_trimmed_keys() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "bypass_local_auth": true, "autorun_program": "  /bin/x %N  ", "web_ui_address": " 0.0.0.0 ",
                "announce_ip": " 1.2.3.4 ", "current_interface_address": "nope",
            })),
        )
        .unwrap();
        assert!(p.webui.bypass_local_auth);
        assert_eq!(to_json(&p)["bypass_local_auth"], true);
        assert_eq!(p.downloads.autorun_program, "/bin/x %N");
        assert_eq!(p.webui.web_ui_address, "0.0.0.0");
        assert_eq!(p.advanced.announce_ip, "1.2.3.4");
        assert_eq!(p.advanced.current_interface_address, "");
    }

    #[test]
    fn read_only_keys_are_ignored() {
        let mut p = Preferences::default();
        apply(
            &mut p,
            &obj(json!({
                "web_ui_api_key": "qbt_aaaaaaaaaaaaaaaaaaaaaaaaaaaa", "add_trackers_url_list": "x",
                "current_interface_name": "en0", "max_ratio_enabled": true,
            })),
        )
        .unwrap();
        assert_eq!(p, Preferences::default());
    }

    #[test]
    fn credential_validation() {
        let mut p = Preferences::default();
        let e = apply(&mut p, &obj(json!({"web_ui_username": "ab"}))).unwrap_err();
        assert_eq!(e, QbtError::invalid("WebUI username must be at least 3 characters long"));
        let e = apply(&mut p, &obj(json!({"web_ui_username": "a:bc"}))).unwrap_err();
        assert_eq!(e, QbtError::invalid("WebUI username cannot contain a colon"));
        let e = apply(&mut p, &obj(json!({"web_ui_password": "12345"}))).unwrap_err();
        assert_eq!(e, QbtError::invalid("WebUI password must be at least 6 characters long"));
        // Keys processed before the failing one are already applied.
        let mut p = Preferences::default();
        let r = apply(&mut p, &obj(json!({"dht": false, "web_ui_username": "x"})));
        assert!(r.is_err());
        assert!(!p.bittorrent.dht);

        let mut p = Preferences::default();
        apply(&mut p, &obj(json!({"web_ui_username": "user", "web_ui_password": "secret1"}))).unwrap();
        assert_eq!(p.webui.web_ui_username, "user");
        assert!(crate::webui::password::verify_password(&p.webui.web_ui_password_pbkdf2, "secret1"));
        assert!(to_json(&p).get("web_ui_password").is_none());
    }

    #[test]
    fn int_parsing_like_qstring() {
        assert_eq!(parse_int_str(" 12 "), Some(12));
        assert_eq!(parse_int_str("-3"), Some(-3));
        assert_eq!(parse_int_str("+4"), Some(4));
        assert_eq!(parse_int_str("1.5"), None);
        assert_eq!(parse_int_str(""), None);
        assert_eq!(parse_int_str("-"), None);
        assert_eq!(parse_int_str("99999999999999999999999"), None);
        assert_eq!(to_int(&json!(3_000_000_000_i64)), 0);
        assert_eq!(to_i64(&json!(3_000_000_000_i64)), 3_000_000_000);
    }
}
