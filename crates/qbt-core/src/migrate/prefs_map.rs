//! Preferences: qBittorrent's `app/preferences` JSON (live WebAPI) or the
//! same object rebuilt from `qBittorrent.ini`, applied through our own
//! WebAPI mapping (`setPreferences` semantics), plus the settings the
//! WebAPI doesn't carry (password hash, WebUI switch, ...).

use serde_json::{Map, Value, json};

use super::ini::{IniFile, QValue, Variant};
use crate::prefs::{Preferences, PreferencesStore, WatchedFolder};

#[derive(Clone, Copy, Debug)]
enum Conv {
    Bool,
    /// The ini stores the negation (`DisableAutoTMMByDefault`, `LocalHostAuth`, ...).
    InvBool,
    Int,
    Float,
    Str,
    /// `QStringList` in the ini, `\n`-joined in the API.
    Lines,
    /// KiB/s in the ini, B/s in the API.
    KibToBytes,
    /// Bytes in the ini, KiB in the API.
    BytesToKib,
    /// Qt meta-enum key name (or a number) to the API integer.
    Enum(&'static [(&'static str, i64)]),
    /// `Net::ProxyType` name (older versions: an integer).
    ProxyType,
}

const SHARE_ACTIONS: &[(&str, i64)] = &[("Stop", 0), ("Remove", 1), ("EnableSuperSeeding", 2), ("RemoveWithContent", 3), ("Default", 0)];
const DAYS: &[(&str, i64)] = &[
    ("EveryDay", 0),
    ("Weekday", 1),
    ("Weekend", 2),
    ("Monday", 3),
    ("Tuesday", 4),
    ("Wednesday", 5),
    ("Thursday", 6),
    ("Friday", 7),
    ("Saturday", 8),
    ("Sunday", 9),
];

use Conv::*;

/// API key, ini key, conversion (docs/spec/preferences.md "ini key" column).
const INI_MAP: &[(&str, &str, Conv)] = &[
    ("locale", "Preferences/General/Locale", Str),
    ("performance_warning", "BitTorrent/Session/PerformanceWarning", Bool),
    ("status_bar_external_ip", "Preferences/General/StatusbarExternalIPDisplayed", Bool),
    ("confirm_torrent_deletion", "Preferences/Advanced/confirmTorrentDeletion", Bool),
    ("file_log_enabled", "Application/FileLogger/Enabled", Bool),
    ("file_log_path", "Application/FileLogger/Path", Str),
    ("file_log_backup_enabled", "Application/FileLogger/Backup", Bool),
    ("file_log_max_size", "Application/FileLogger/MaxSizeBytes", BytesToKib),
    ("file_log_delete_old", "Application/FileLogger/DeleteOld", Bool),
    ("file_log_age", "Application/FileLogger/Age", Int),
    ("file_log_age_type", "Application/FileLogger/AgeType", Int),
    ("delete_torrent_content_files", "Preferences/General/DeleteTorrentsFilesAsDefault", Bool),
    ("torrent_content_layout", "BitTorrent/Session/TorrentContentLayout", Str),
    ("add_to_top_of_queue", "BitTorrent/Session/AddTorrentToTopOfQueue", Bool),
    ("add_stopped_enabled", "BitTorrent/Session/AddTorrentStopped", Bool),
    ("torrent_stop_condition", "BitTorrent/Session/TorrentStopCondition", Str),
    ("merge_trackers", "BitTorrent/MergeTrackersEnabled", Bool),
    ("auto_delete_mode", "Core/AutoDeleteAddedTorrentFile", Enum(&[("Never", 0), ("IfAdded", 1), ("Always", 2)])),
    ("preallocate_all", "BitTorrent/Session/Preallocation", Bool),
    ("incomplete_files_ext", "BitTorrent/Session/AddExtensionToIncompleteFiles", Bool),
    ("use_unwanted_folder", "BitTorrent/Session/UseUnwantedFolder", Bool),
    ("auto_tmm_enabled", "BitTorrent/Session/DisableAutoTMMByDefault", InvBool),
    ("torrent_changed_tmm_enabled", "BitTorrent/Session/DisableAutoTMMTriggers/CategoryChanged", InvBool),
    ("save_path_changed_tmm_enabled", "BitTorrent/Session/DisableAutoTMMTriggers/DefaultSavePathChanged", InvBool),
    ("category_changed_tmm_enabled", "BitTorrent/Session/DisableAutoTMMTriggers/CategorySavePathChanged", InvBool),
    ("save_path", "BitTorrent/Session/DefaultSavePath", Str),
    ("temp_path_enabled", "BitTorrent/Session/TempPathEnabled", Bool),
    ("temp_path", "BitTorrent/Session/TempPath", Str),
    ("use_category_paths_in_manual_mode", "BitTorrent/Session/UseCategoryPathsInManualMode", Bool),
    ("export_dir", "BitTorrent/Session/TorrentExportDirectory", Str),
    ("export_dir_fin", "BitTorrent/Session/FinishedTorrentExportDirectory", Str),
    ("excluded_file_names_enabled", "BitTorrent/ExcludedFileNamesEnabled", Bool),
    ("excluded_file_names", "BitTorrent/Session/ExcludedFileNames", Lines),
    ("mail_notification_enabled", "Preferences/MailNotification/enabled", Bool),
    ("mail_notification_sender", "Preferences/MailNotification/sender", Str),
    ("mail_notification_email", "Preferences/MailNotification/email", Str),
    ("mail_notification_smtp", "Preferences/MailNotification/smtp_server", Str),
    ("mail_notification_ssl_enabled", "Preferences/MailNotification/req_ssl", Bool),
    ("mail_notification_auth_enabled", "Preferences/MailNotification/req_auth", Bool),
    ("mail_notification_username", "Preferences/MailNotification/username", Str),
    ("mail_notification_password", "Preferences/MailNotification/password", Str),
    ("autorun_on_torrent_added_enabled", "AutoRun/OnTorrentAdded/Enabled", Bool),
    ("autorun_on_torrent_added_program", "AutoRun/OnTorrentAdded/Program", Str),
    ("autorun_enabled", "AutoRun/enabled", Bool),
    ("autorun_program", "AutoRun/program", Str),
    ("listen_port", "BitTorrent/Session/Port", Int),
    ("ssl_enabled", "BitTorrent/Session/SSL/Enabled", Bool),
    ("ssl_listen_port", "BitTorrent/Session/SSL/Port", Int),
    ("upnp", "Network/PortForwardingEnabled", Bool),
    ("max_connec", "BitTorrent/Session/MaxConnections", Int),
    ("max_connec_per_torrent", "BitTorrent/Session/MaxConnectionsPerTorrent", Int),
    ("max_uploads", "BitTorrent/Session/MaxUploads", Int),
    ("max_uploads_per_torrent", "BitTorrent/Session/MaxUploadsPerTorrent", Int),
    ("i2p_enabled", "BitTorrent/Session/I2P/Enabled", Bool),
    ("i2p_address", "BitTorrent/Session/I2P/Address", Str),
    ("i2p_port", "BitTorrent/Session/I2P/Port", Int),
    ("i2p_mixed_mode", "BitTorrent/Session/I2P/MixedMode", Bool),
    ("i2p_inbound_quantity", "BitTorrent/Session/I2P/InboundQuantity", Int),
    ("i2p_outbound_quantity", "BitTorrent/Session/I2P/OutboundQuantity", Int),
    ("i2p_inbound_length", "BitTorrent/Session/I2P/InboundLength", Int),
    ("i2p_outbound_length", "BitTorrent/Session/I2P/OutboundLength", Int),
    ("proxy_type", "Network/Proxy/Type", ProxyType),
    ("proxy_ip", "Network/Proxy/IP", Str),
    ("proxy_port", "Network/Proxy/Port", Int),
    ("proxy_auth_enabled", "Network/Proxy/AuthEnabled", Bool),
    ("proxy_username", "Network/Proxy/Username", Str),
    ("proxy_password", "Network/Proxy/Password", Str),
    ("proxy_hostname_lookup", "Network/Proxy/HostnameLookupEnabled", Bool),
    ("proxy_bittorrent", "Network/Proxy/Profiles/BitTorrent", Bool),
    ("proxy_peer_connections", "BitTorrent/Session/ProxyPeerConnections", Bool),
    ("proxy_rss", "Network/Proxy/Profiles/RSS", Bool),
    ("proxy_misc", "Network/Proxy/Profiles/Misc", Bool),
    ("ip_filter_enabled", "BitTorrent/Session/IPFilteringEnabled", Bool),
    ("ip_filter_path", "BitTorrent/Session/IPFilter", Str),
    ("ip_filter_trackers", "BitTorrent/Session/TrackerFilteringEnabled", Bool),
    ("banned_IPs", "State/BannedIPs", Lines),
    ("dl_limit", "BitTorrent/Session/GlobalDLSpeedLimit", KibToBytes),
    ("up_limit", "BitTorrent/Session/GlobalUPSpeedLimit", KibToBytes),
    ("alt_dl_limit", "BitTorrent/Session/AlternativeGlobalDLSpeedLimit", KibToBytes),
    ("alt_up_limit", "BitTorrent/Session/AlternativeGlobalUPSpeedLimit", KibToBytes),
    ("bittorrent_protocol", "BitTorrent/Session/BTProtocol", Enum(&[("Both", 0), ("TCP", 1), ("UTP", 2)])),
    ("limit_utp_rate", "BitTorrent/Session/uTPRateLimited", Bool),
    ("limit_tcp_overhead", "BitTorrent/Session/IncludeOverheadInLimits", Bool),
    ("limit_lan_peers", "BitTorrent/Session/IgnoreLimitsOnLAN", InvBool),
    ("scheduler_enabled", "BitTorrent/Session/BandwidthSchedulerEnabled", Bool),
    ("scheduler_days", "Preferences/Scheduler/days", Enum(DAYS)),
    ("dht", "BitTorrent/Session/DHTEnabled", Bool),
    ("pex", "BitTorrent/Session/PeXEnabled", Bool),
    ("lsd", "BitTorrent/Session/LSDEnabled", Bool),
    ("encryption", "BitTorrent/Session/Encryption", Int),
    ("anonymous_mode", "BitTorrent/Session/AnonymousModeEnabled", Bool),
    ("max_active_checking_torrents", "BitTorrent/Session/MaxActiveCheckingTorrents", Int),
    ("queueing_enabled", "BitTorrent/Session/QueueingSystemEnabled", Bool),
    ("max_active_downloads", "BitTorrent/Session/MaxActiveDownloads", Int),
    ("max_active_torrents", "BitTorrent/Session/MaxActiveTorrents", Int),
    ("max_active_uploads", "BitTorrent/Session/MaxActiveUploads", Int),
    ("dont_count_slow_torrents", "BitTorrent/Session/IgnoreSlowTorrentsForQueueing", Bool),
    ("slow_torrent_dl_rate_threshold", "BitTorrent/Session/SlowTorrentsDownloadRate", Int),
    ("slow_torrent_ul_rate_threshold", "BitTorrent/Session/SlowTorrentsUploadRate", Int),
    ("slow_torrent_inactive_timer", "BitTorrent/Session/SlowTorrentsInactivityTimer", Int),
    ("max_ratio", "BitTorrent/Session/GlobalMaxRatio", Float),
    ("max_seeding_time", "BitTorrent/Session/GlobalMaxSeedingMinutes", Int),
    ("max_inactive_seeding_time", "BitTorrent/Session/GlobalMaxInactiveSeedingMinutes", Int),
    ("max_ratio_act", "BitTorrent/Session/ShareLimitAction", Enum(SHARE_ACTIONS)),
    ("add_trackers_enabled", "BitTorrent/Session/AddTrackersEnabled", Bool),
    ("add_trackers", "BitTorrent/Session/AdditionalTrackers", Str),
    ("add_trackers_from_url_enabled", "BitTorrent/Session/AddTrackersFromURLEnabled", Bool),
    ("add_trackers_url", "BitTorrent/Session/AdditionalTrackersURL", Str),
    ("web_ui_domain_list", "Preferences/WebUI/ServerDomains", Str),
    ("web_ui_address", "Preferences/WebUI/Address", Str),
    ("web_ui_port", "Preferences/WebUI/Port", Int),
    ("web_ui_upnp", "Preferences/WebUI/UseUPnP", Bool),
    ("use_https", "Preferences/WebUI/HTTPS/Enabled", Bool),
    ("web_ui_https_cert_path", "Preferences/WebUI/HTTPS/CertificatePath", Str),
    ("web_ui_https_key_path", "Preferences/WebUI/HTTPS/KeyPath", Str),
    ("web_ui_username", "Preferences/WebUI/Username", Str),
    ("bypass_local_auth", "Preferences/WebUI/LocalHostAuth", InvBool),
    ("bypass_auth_subnet_whitelist_enabled", "Preferences/WebUI/AuthSubnetWhitelistEnabled", Bool),
    ("bypass_auth_subnet_whitelist", "Preferences/WebUI/AuthSubnetWhitelist", Lines),
    ("web_ui_max_auth_fail_count", "Preferences/WebUI/MaxAuthenticationFailCount", Int),
    ("web_ui_ban_duration", "Preferences/WebUI/BanDuration", Int),
    ("web_ui_session_timeout", "Preferences/WebUI/SessionTimeout", Int),
    ("alternative_webui_enabled", "Preferences/WebUI/AlternativeUIEnabled", Bool),
    ("alternative_webui_path", "Preferences/WebUI/RootFolder", Str),
    ("web_ui_clickjacking_protection_enabled", "Preferences/WebUI/ClickjackingProtection", Bool),
    ("web_ui_csrf_protection_enabled", "Preferences/WebUI/CSRFProtection", Bool),
    ("web_ui_secure_cookie_enabled", "Preferences/WebUI/SecureCookie", Bool),
    ("web_ui_host_header_validation_enabled", "Preferences/WebUI/HostHeaderValidation", Bool),
    ("web_ui_use_custom_http_headers_enabled", "Preferences/WebUI/CustomHTTPHeadersEnabled", Bool),
    ("web_ui_custom_http_headers", "Preferences/WebUI/CustomHTTPHeaders", Str),
    ("web_ui_reverse_proxy_enabled", "Preferences/WebUI/ReverseProxySupportEnabled", Bool),
    ("web_ui_reverse_proxies_list", "Preferences/WebUI/TrustedReverseProxiesList", Str),
    ("dyndns_enabled", "Preferences/DynDNS/Enabled", Bool),
    ("dyndns_service", "Preferences/DynDNS/Service", Enum(&[("DynDNS", 0), ("NoIP", 1), ("None", -1)])),
    ("dyndns_username", "Preferences/DynDNS/Username", Str),
    ("dyndns_password", "Preferences/DynDNS/Password", Str),
    ("dyndns_domain", "Preferences/DynDNS/DomainName", Str),
    ("rss_refresh_interval", "RSS/Session/RefreshInterval", Int),
    ("rss_fetch_delay", "RSS/Session/FetchDelay", Int),
    ("rss_max_articles_per_feed", "RSS/Session/MaxArticlesPerFeed", Int),
    ("rss_processing_enabled", "RSS/Session/EnableProcessing", Bool),
    ("rss_auto_downloading_enabled", "RSS/AutoDownloader/EnableProcessing", Bool),
    ("rss_download_repack_proper_episodes", "RSS/AutoDownloader/DownloadRepacks", Bool),
    ("rss_smart_episode_filters", "RSS/AutoDownloader/SmartEpisodeFilter", Lines),
    ("resume_data_storage_type", "BitTorrent/Session/ResumeDataStorageType", Str),
    ("torrent_content_remove_option", "BitTorrent/Session/TorrentContentRemoveOption", Str),
    ("memory_working_set_limit", "Application/MemoryWorkingSetLimit", Int),
    ("current_network_interface", "BitTorrent/Session/Interface", Str),
    ("current_interface_address", "BitTorrent/Session/InterfaceAddress", Str),
    ("save_resume_data_interval", "BitTorrent/Session/SaveResumeDataInterval", Int),
    ("save_statistics_interval", "BitTorrent/Session/SaveStatisticsInterval", Int),
    ("torrent_file_size_limit", "BitTorrent/TorrentFileSizeLimit", Int),
    ("confirm_torrent_recheck", "Preferences/Advanced/confirmTorrentRecheck", Bool),
    ("recheck_completed_torrents", "Preferences/Advanced/RecheckOnCompletion", Bool),
    ("app_instance_name", "Application/InstanceName", Str),
    ("refresh_interval", "BitTorrent/Session/RefreshInterval", Int),
    ("resolve_peer_host_names", "Preferences/Connection/ResolvePeerHostNames", Bool),
    ("resolve_peer_countries", "Preferences/Connection/ResolvePeerCountries", Bool),
    ("reannounce_when_address_changed", "BitTorrent/Session/ReannounceWhenAddressChanged", Bool),
    ("enable_embedded_tracker", "BitTorrent/TrackerEnabled", Bool),
    ("embedded_tracker_port", "Preferences/Advanced/trackerPort", Int),
    ("embedded_tracker_port_forwarding", "Preferences/Advanced/trackerPortForwarding", Bool),
    ("mark_of_the_web", "Preferences/Advanced/markOfTheWeb", Bool),
    ("ignore_ssl_errors", "Preferences/Advanced/IgnoreSSLErrors", Bool),
    ("python_executable_path", "Preferences/Search/pythonExecutablePath", Str),
    ("bdecode_depth_limit", "BitTorrent/BdecodeDepthLimit", Int),
    ("bdecode_token_limit", "BitTorrent/BdecodeTokenLimit", Int),
    ("async_io_threads", "BitTorrent/Session/AsyncIOThreadsCount", Int),
    ("hashing_threads", "BitTorrent/Session/HashingThreadsCount", Int),
    ("file_pool_size", "BitTorrent/Session/FilePoolSize", Int),
    ("checking_memory_use", "BitTorrent/Session/CheckingMemUsageSize", Int),
    ("disk_cache", "BitTorrent/Session/DiskCacheSize", Int),
    ("disk_cache_ttl", "BitTorrent/Session/DiskCacheTTL", Int),
    ("disk_queue_size", "BitTorrent/Session/DiskQueueSize", Int),
    ("disk_io_type", "BitTorrent/Session/DiskIOType", Enum(&[("Default", 0), ("MMap", 1), ("Posix", 2), ("SimplePreadPwrite", 3)])),
    ("disk_io_read_mode", "BitTorrent/Session/DiskIOReadMode", Enum(&[("DisableOSCache", 0), ("EnableOSCache", 1)])),
    (
        "disk_io_write_mode",
        "BitTorrent/Session/DiskIOWriteMode",
        Enum(&[("DisableOSCache", 0), ("EnableOSCache", 1), ("WriteThrough", 2)]),
    ),
    ("enable_coalesce_read_write", "BitTorrent/Session/CoalesceReadWrite", Bool),
    ("enable_piece_extent_affinity", "BitTorrent/Session/PieceExtentAffinity", Bool),
    ("enable_upload_suggestions", "BitTorrent/Session/SuggestMode", Bool),
    ("send_buffer_watermark", "BitTorrent/Session/SendBufferWatermark", Int),
    ("send_buffer_low_watermark", "BitTorrent/Session/SendBufferLowWatermark", Int),
    ("send_buffer_watermark_factor", "BitTorrent/Session/SendBufferWatermarkFactor", Int),
    ("connection_speed", "BitTorrent/Session/ConnectionSpeed", Int),
    ("socket_send_buffer_size", "BitTorrent/Session/SocketSendBufferSize", Int),
    ("socket_receive_buffer_size", "BitTorrent/Session/SocketReceiveBufferSize", Int),
    ("socket_backlog_size", "BitTorrent/Session/SocketBacklogSize", Int),
    ("outgoing_ports_min", "BitTorrent/Session/OutgoingPortsMin", Int),
    ("outgoing_ports_max", "BitTorrent/Session/OutgoingPortsMax", Int),
    ("upnp_lease_duration", "BitTorrent/Session/UPnPLeaseDuration", Int),
    ("peer_tos", "BitTorrent/Session/PeerToS", Int),
    ("utp_tcp_mixed_mode", "BitTorrent/Session/uTPMixedMode", Enum(&[("TCP", 0), ("Proportional", 1)])),
    ("hostname_cache_ttl", "BitTorrent/Session/HostnameCacheTTL", Int),
    ("idn_support_enabled", "BitTorrent/Session/IDNSupportEnabled", Bool),
    ("enable_multi_connections_from_same_ip", "BitTorrent/Session/MultiConnectionsPerIp", Bool),
    ("validate_https_tracker_certificate", "BitTorrent/Session/ValidateHTTPSTrackerCertificate", Bool),
    ("ssrf_mitigation", "BitTorrent/Session/SSRFMitigation", Bool),
    ("block_peers_on_privileged_ports", "BitTorrent/Session/BlockPeersOnPrivilegedPorts", Bool),
    ("upload_slots_behavior", "BitTorrent/Session/ChokingAlgorithm", Enum(&[("FixedSlots", 0), ("RateBased", 1)])),
    (
        "upload_choking_algorithm",
        "BitTorrent/Session/SeedChokingAlgorithm",
        Enum(&[("RoundRobin", 0), ("FastestUpload", 1), ("AntiLeech", 2)]),
    ),
    ("announce_to_all_trackers", "BitTorrent/Session/AnnounceToAllTrackers", Bool),
    ("announce_to_all_tiers", "BitTorrent/Session/AnnounceToAllTiers", Bool),
    ("announce_ip", "BitTorrent/Session/AnnounceIP", Str),
    ("announce_port", "BitTorrent/Session/AnnouncePort", Int),
    ("max_concurrent_http_announces", "BitTorrent/Session/MaxConcurrentHTTPAnnounces", Int),
    ("stop_tracker_timeout", "BitTorrent/Session/StopTrackerTimeout", Int),
    ("peer_turnover", "BitTorrent/Session/PeerTurnover", Int),
    ("peer_turnover_cutoff", "BitTorrent/Session/PeerTurnoverCutOff", Int),
    ("peer_turnover_interval", "BitTorrent/Session/PeerTurnoverInterval", Int),
    ("request_queue_size", "BitTorrent/Session/RequestQueueSize", Int),
    ("dht_bootstrap_nodes", "BitTorrent/Session/DHTBootstrapNodes", Str),
];

/// ini keys holding the scheduler times (`QTime`).
const SCHEDULER_START: &str = "Preferences/Scheduler/start_time";
const SCHEDULER_END: &str = "Preferences/Scheduler/end_time";

/// Settings stored next to the WebAPI ones but not part of it.
pub const EXTRA_KEYS: &[&str] = &[
    "Preferences/WebUI/Enabled",
    "Preferences/WebUI/Password_PBKDF2",
    "Preferences/WebUI/APIKey",
    "BitTorrent/Session/StartPaused",
    "BitTorrent/Session/SubcategoriesEnabled",
    "BitTorrent/Session/ShutdownTimeout",
    "BitTorrent/Session/InterfaceName",
];

fn enum_value(v: &QValue, names: &[(&str, i64)]) -> Option<i64> {
    let s = v.to_string_value();
    let t = s.trim();
    if let Ok(n) = t.parse::<i64>() {
        return Some(n);
    }
    names.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)).map(|(_, i)| *i)
}

fn proxy_type(v: &QValue) -> Option<String> {
    let s = v.to_string_value();
    let t = s.trim();
    Some(match t.parse::<i64>() {
        // Old integer values (`Net::ProxyType` before the names; 3/4 were the
        // "with authentication" variants).
        Ok(0) | Ok(-1) => "None".into(),
        Ok(1) | Ok(3) => "HTTP".into(),
        Ok(2) | Ok(4) => "SOCKS5".into(),
        Ok(5) => "SOCKS4".into(),
        Ok(_) => return None,
        Err(_) => match t.to_ascii_uppercase().as_str() {
            "NONE" | "" => "None".into(),
            "HTTP" => "HTTP".into(),
            "SOCKS5" => "SOCKS5".into(),
            "SOCKS4" => "SOCKS4".into(),
            _ => return None,
        },
    })
}

fn qtime(v: &QValue) -> Option<(i64, i64)> {
    match v {
        QValue::Variant(Variant::Time(Some(ms))) => Some((i64::from(ms / 3_600_000), i64::from((ms / 60_000) % 60))),
        other => {
            // "HH:mm" or "HH:mm:ss" if a tool wrote it as text.
            let s = other.to_string_value();
            let mut it = s.trim().split(':');
            let h = it.next()?.parse::<i64>().ok()?;
            let m = it.next()?.parse::<i64>().ok()?;
            ((0..24).contains(&h) && (0..60).contains(&m)).then_some((h, m))
        }
    }
}

/// The `app/preferences` object qBittorrent would report for this ini
/// (only keys present in the file), and the ini keys it used.
pub fn api_json_from_ini(ini: &IniFile) -> (Map<String, Value>, Vec<String>) {
    let mut m = Map::new();
    let mut used = Vec::new();
    for &(api, key, conv) in INI_MAP {
        let Some(v) = ini.get(key) else { continue };
        let value = match conv {
            Bool => v.to_bool().map(Value::from),
            InvBool => v.to_bool().map(|b| Value::from(!b)),
            Int => v.to_int().map(Value::from),
            Float => v.to_double().map(Value::from),
            Str => Some(Value::from(v.to_string_value())),
            Lines => Some(Value::from(v.to_string_list().join("\n"))),
            KibToBytes => v.to_int().map(|k| Value::from(k.max(0) * 1024)),
            BytesToKib => v.to_int().map(|b| Value::from(b / 1024)),
            Enum(names) => enum_value(v, names).map(Value::from),
            ProxyType => proxy_type(v).map(Value::from),
        };
        if let Some(value) = value {
            m.insert(api.to_owned(), value);
            used.push(key.to_owned());
        }
    }
    for (key, h, mi) in [(SCHEDULER_START, "schedule_from_hour", "schedule_from_min"), (SCHEDULER_END, "schedule_to_hour", "schedule_to_min")] {
        if let Some((hour, min)) = ini.get(key).and_then(qtime) {
            m.insert(h.into(), json!(hour));
            m.insert(mi.into(), json!(min));
            used.push(key.to_owned());
        }
    }
    // Pre-4.6 name of the share limit action.
    if !m.contains_key("max_ratio_act")
        && let Some(v) = ini.int("BitTorrent/Session/MaxRatioAction")
    {
        m.insert("max_ratio_act".into(), json!(v.max(0)));
        used.push("BitTorrent/Session/MaxRatioAction".into());
    }
    (m, used)
}

/// A WebUI password hash as stored by qBittorrent (`@ByteArray(salt:hash)`
/// in the ini): the `salt:hash` text, if it looks like one.
pub fn password_hash(ini: &IniFile) -> Option<String> {
    let raw = ini.byte_array("Preferences/WebUI/Password_PBKDF2")?;
    let s = String::from_utf8(raw).ok()?;
    let s = s.trim();
    let (salt, hash) = s.split_once(':')?;
    let b64 = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    (b64(salt) && b64(hash)).then(|| s.to_owned())
}

pub struct Built {
    pub prefs: Preferences,
    /// Notes for the report (never secrets).
    pub notes: Vec<String>,
}

/// Applies `api` (qBittorrent's `app/preferences` object, from the WebAPI
/// or [`api_json_from_ini`]) onto our defaults, then the ini-only
/// settings and the full watched-folder model.
pub fn build(api: &Map<String, Value>, ini: &IniFile, watched: Option<Vec<WatchedFolder>>, source_data_dir: &str) -> Result<Built, String> {
    let mut notes = Vec::new();
    let mut api = api.clone();
    // GET-only or derived keys our setPreferences ignores (set below).
    for k in ["add_trackers_url_list", "web_ui_api_key", "current_interface_name"] {
        api.remove(k);
    }
    let store = PreferencesStore::in_memory(Preferences::default());
    store.apply_json(&Value::Object(api.clone()).to_string()).map_err(|e| format!("preferences rejected: {e}"))?;
    let mut p = store.get();

    if let Some(v) = ini.bool("Preferences/WebUI/Enabled") {
        p.webui.web_ui_enabled = v;
    }
    match password_hash(ini) {
        Some(h) => {
            p.webui.web_ui_password_pbkdf2 = h;
            notes.push("WebUI password hash copied from qBittorrent.ini".into());
        }
        None if ini.contains("Preferences/WebUI/Password_PBKDF2") => {
            notes.push("WebUI password hash in qBittorrent.ini is not in PBKDF2 salt:hash form; not copied".into())
        }
        None => notes.push("no WebUI password set in qBittorrent.ini (a temporary one is generated at start)".into()),
    }
    if let Some(k) = ini.string("Preferences/WebUI/APIKey").filter(|k| !k.is_empty()) {
        if k.starts_with("qbt_") && k.len() == 32 && k.is_ascii() {
            p.webui.web_ui_api_key = k;
            notes.push("WebUI API key copied".into());
        } else {
            notes.push("WebUI API key has an unexpected format; not copied".into());
        }
    }
    if let Some(v) = ini.bool("BitTorrent/Session/StartPaused") {
        p.advanced.start_paused = v;
    }
    if let Some(v) = ini.bool("BitTorrent/Session/SubcategoriesEnabled") {
        p.downloads.use_subcategories = v;
    }
    if let Some(v) = ini.int("BitTorrent/Session/ShutdownTimeout") {
        p.advanced.shutdown_timeout = v;
    }
    if let Some(v) = ini.string("BitTorrent/Session/InterfaceName") {
        p.advanced.current_interface_name = v;
    }
    if let Some(folders) = watched {
        p.downloads.watched_folders = folders;
    }
    // The log folder inside qBittorrent's own data folder is its default:
    // use ours (the profile's data/logs) rather than writing there.
    let log = p.behavior.file_log_path.trim_end_matches('/');
    let src = source_data_dir.trim_end_matches('/');
    if !log.is_empty() && !src.is_empty() && (log == src || log.starts_with(&format!("{src}/"))) {
        notes.push(format!("log folder \"{log}\" is inside qBittorrent's data folder: using the profile's data/logs instead"));
        p.behavior.file_log_path.clear();
    }
    Ok(Built { prefs: p, notes })
}

/// qBittorrent's `watched_folders.json`: `{ path: { recursive, add_torrent_params } }`.
pub fn parse_watched_folders(data: &[u8]) -> Result<Vec<WatchedFolder>, String> {
    let v: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    let mut out = Vec::new();
    for (path, o) in obj {
        if path.trim().is_empty() {
            continue;
        }
        let recursive = o.get("recursive").and_then(Value::as_bool).unwrap_or(false);
        let params = match o.get("add_torrent_params") {
            Some(p) => serde_json::from_value(p.clone()).map_err(|e| format!("{path}: {e}"))?,
            None => Default::default(),
        };
        out.push(WatchedFolder { path: path.clone(), recursive, params });
    }
    Ok(out)
}

/// Why a key reads differently in our `app/preferences` than in theirs.
pub fn known_difference(key: &str) -> Option<&'static str> {
    Some(match key {
        "add_trackers_url_list" => "read-only: the list downloaded from add_trackers_url, fetched again at start",
        "current_interface_name" => "derived from the selected network interface",
        "file_log_path" => "qBittorrent's default log folder is inside its own data folder; the profile's data/logs is used",
        "locale" => "an empty locale reads back as the system locale",
        "memory_working_set_limit" | "disk_cache" | "disk_cache_ttl" | "enable_coalesce_read_write" | "disk_io_type" => {
            "libtorrent tuning value normalised by our engine settings"
        }
        "hashing_threads" => "clamped to 1..1024",
        "bypass_auth_subnet_whitelist" => "subnets are normalised (CIDR form)",
        "banned_IPs" => "addresses are normalised, sorted and de-duplicated",
        _ => return None,
    })
}

/// Keys whose values differ between `theirs` and `ours` (both
/// `app/preferences` objects). Secrets are never part of the result values.
pub fn diff(theirs: &Map<String, Value>, ours: &Map<String, Value>) -> Vec<(String, String, String)> {
    let secret = |k: &str| k.contains("password") || k == "web_ui_api_key";
    let show = |k: &str, v: Option<&Value>| match v {
        None => "(absent)".to_owned(),
        Some(_) if secret(k) => "(secret)".to_owned(),
        Some(v) => {
            let s = v.to_string();
            if s.chars().count() > 80 { format!("{}...", s.chars().take(77).collect::<String>()) } else { s }
        }
    };
    let mut keys: Vec<&String> = theirs.keys().chain(ours.keys()).collect();
    keys.sort();
    keys.dedup();
    let mut out = Vec::new();
    for k in keys {
        let (a, b) = (theirs.get(k), ours.get(k));
        let same = match (a, b) {
            (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
            (x, y) => x == y,
        };
        if !same {
            out.push((k.clone(), show(k, a), show(k, b)));
        }
    }
    out
}

/// Ini keys this migration doesn't carry over, grouped for the report.
pub fn classify_unused(key: &str) -> &'static str {
    let k = key.to_ascii_lowercase();
    if k.contains("geometry") || k.contains("headerstate") || k.contains("splitter") || k.ends_with("/size") || k.contains("dialogsize") || k.contains("state=") {
        "window/dialog geometry and column layout (Qt-specific)"
    } else if k.starts_with("meta/") || k.starts_with("legalnotice/") {
        "qBittorrent bookkeeping"
    } else if k.starts_with("appearance/") || k.contains("customuitheme") || k.contains("iconsinmenus") || k.contains("noSplashScreen".to_ascii_lowercase().as_str()) {
        "Qt look and feel (no macOS equivalent)"
    } else if k.starts_with("stats/") {
        "all-time statistics (carried over to data/stats.json)"
    } else if k.starts_with("searchengines/") {
        "search plugin state (carried over to config/search.json)"
    } else {
        "other GUI state without an equivalent"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ini(text: &str) -> IniFile {
        IniFile::parse(text.as_bytes())
    }

    #[test]
    fn ini_to_api_conversions() {
        let f = ini(concat!(
            "[Application]\nFileLogger\\MaxSizeBytes=66560\n",
            "[BitTorrent]\n",
            "Session\\Port=52739\n",
            "Session\\DisableAutoTMMByDefault=false\n",
            "Session\\GlobalDLSpeedLimit=100\n",
            "Session\\BTProtocol=TCP\n",
            "Session\\ShareLimitAction=Stop\n",
            "Session\\ExcludedFileNames=*.nfo, *.txt\n",
            "Session\\TorrentContentLayout=NoSubfolder\n",
            "Session\\DHTBootstrapNodes=\"a:1, b:2\"\n",
            "Session\\GlobalMaxRatio=1.5\n",
            "Session\\IgnoreLimitsOnLAN=true\n",
            "Session\\DiskIOType=Posix\n",
            "[Core]\nAutoDeleteAddedTorrentFile=IfAdded\n",
            "[Network]\nProxy\\Type=SOCKS5\n",
            "[Preferences]\n",
            "WebUI\\LocalHostAuth=false\n",
            "Scheduler\\start_time=@Variant(\\0\\0\\0\\xf\\x1\\xb7t\\0)\n",
            "Scheduler\\days=Weekend\n",
            "DynDNS\\Service=NoIP\n",
            "[RSS]\nAutoDownloader\\SmartEpisodeFilter=s(\\\\d+)e(\\\\d+), \"(\\\\d{1,2})\"\n",
        ));
        let (m, used) = api_json_from_ini(&f);
        assert_eq!(m["file_log_max_size"], json!(65));
        assert_eq!(m["listen_port"], json!(52739));
        assert_eq!(m["auto_tmm_enabled"], json!(true));
        assert_eq!(m["dl_limit"], json!(102_400));
        assert_eq!(m["bittorrent_protocol"], json!(1));
        assert_eq!(m["max_ratio_act"], json!(0));
        assert_eq!(m["excluded_file_names"], json!("*.nfo\n*.txt"));
        assert_eq!(m["torrent_content_layout"], json!("NoSubfolder"));
        assert_eq!(m["dht_bootstrap_nodes"], json!("a:1, b:2"));
        assert_eq!(m["max_ratio"], json!(1.5));
        assert_eq!(m["limit_lan_peers"], json!(false));
        assert_eq!(m["disk_io_type"], json!(2));
        assert_eq!(m["auto_delete_mode"], json!(1));
        assert_eq!(m["proxy_type"], json!("SOCKS5"));
        assert_eq!(m["bypass_local_auth"], json!(true));
        assert_eq!((m["schedule_from_hour"].clone(), m["schedule_from_min"].clone()), (json!(8), json!(0)));
        assert_eq!(m["scheduler_days"], json!(2));
        assert_eq!(m["dyndns_service"], json!(1));
        assert_eq!(m["rss_smart_episode_filters"], json!("s(\\d+)e(\\d+)\n(\\d{1,2})"));
        assert!(!m.contains_key("upnp"), "absent keys keep our defaults");
        assert!(used.contains(&"BitTorrent/Session/Port".to_string()));
    }

    #[test]
    fn builds_preferences_with_extras() {
        let hash = crate::webui::password::hash_password("migrated-secret");
        let text = format!(
            concat!(
                "[BitTorrent]\nSession\\Port=52739\nSession\\StartPaused=true\nSession\\SubcategoriesEnabled=true\n",
                "[Application]\nFileLogger\\Path=/src/data/logs\n",
                "[Preferences]\nWebUI\\Enabled=true\nWebUI\\Port=9292\nWebUI\\Username=admin\n",
                "WebUI\\Password_PBKDF2=\"@ByteArray({})\"\nWebUI\\APIKey=qbt_0123456789abcdefghijklmnopqr\n",
            ),
            hash
        );
        let f = ini(&text);
        let (api, _) = api_json_from_ini(&f);
        let built = build(&api, &f, None, "/src/data").unwrap();
        let p = built.prefs;
        assert_eq!(p.connection.listen_port, 52739);
        assert_eq!(p.webui.web_ui_port, 9292);
        assert_eq!(p.webui.web_ui_username, "admin");
        assert!(p.webui.web_ui_enabled && p.advanced.start_paused && p.downloads.use_subcategories);
        assert_eq!(p.webui.web_ui_password_pbkdf2, hash);
        assert!(crate::webui::password::verify_password(&p.webui.web_ui_password_pbkdf2, "migrated-secret"));
        assert!(!crate::webui::password::verify_password(&p.webui.web_ui_password_pbkdf2, "wrong"));
        assert_eq!(p.webui.web_ui_api_key, "qbt_0123456789abcdefghijklmnopqr");
        assert_eq!(p.behavior.file_log_path, "", "qBittorrent's own log folder is not reused");
        assert!(built.notes.iter().all(|n| !n.contains(&hash)), "no secrets in notes");

        // Malformed hashes are refused.
        let f = ini("[Preferences]\nWebUI\\Password_PBKDF2=\"@ByteArray(not a hash)\"\n");
        assert_eq!(password_hash(&f), None);
        // An invalid username is an error, like setPreferences.
        let f = ini("[Preferences]\nWebUI\\Username=ab\n");
        let (api, _) = api_json_from_ini(&f);
        assert!(build(&api, &f, None, "").is_err());
    }

    #[test]
    fn watched_folders_and_diff() {
        let data = br#"{"/w": {"recursive": true, "add_torrent_params": {"category": "Movies", "use_auto_tmm": false,
            "save_path": "/m", "operating_mode": "AutoManaged", "ratio_limit": -2, "tags": ["a"], "stopped": true}}}"#;
        let w = parse_watched_folders(data).unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].recursive);
        assert_eq!(w[0].params.category, "Movies");
        assert_eq!(w[0].params.auto_tmm, Some(false));
        assert_eq!(w[0].params.stopped, Some(true));
        assert!(parse_watched_folders(b"[]").is_err());

        let a: Map<String, Value> = serde_json::from_str(r#"{"x":1,"y":"a","proxy_password":"s","z":1.0}"#).unwrap();
        let b: Map<String, Value> = serde_json::from_str(r#"{"x":1,"y":"b","proxy_password":"t","z":1}"#).unwrap();
        let d = diff(&a, &b);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], ("proxy_password".into(), "(secret)".into(), "(secret)".into()));
        assert_eq!(d[1].0, "y");
    }
}
