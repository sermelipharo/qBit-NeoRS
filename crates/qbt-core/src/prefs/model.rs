//! The preferences model. Field names follow the WebAPI keys of qBittorrent
//! 5.2.4 (`/api/v2/app/preferences`); values use WebAPI units (speed limits in
//! bytes/s, times in the unit noted per field). Defaults are qBittorrent's.
//!
//! Persistence uses serde with `#[serde(default)]` everywhere so new fields
//! never break old files. The exact WebAPI JSON view (ordering, derived keys,
//! partial updates) lives in `api_json.rs`.

use serde::{Deserialize, Serialize};

use super::enums::*;
use crate::model::{ContentLayout, ShareLimitAction, StopCondition, TorrentParamsTemplate};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct Preferences {
    pub behavior: BehaviorPrefs,
    pub downloads: DownloadsPrefs,
    pub connection: ConnectionPrefs,
    pub speed: SpeedPrefs,
    pub bittorrent: BitTorrentPrefs,
    pub rss: RssPrefs,
    pub webui: WebUiPrefs,
    pub advanced: AdvancedPrefs,
}

/// Behavior page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct BehaviorPrefs {
    /// UI language code, "" = system.
    pub locale: String,
    pub performance_warning: bool,
    pub status_bar_external_ip: bool,
    pub confirm_torrent_deletion: bool,
    pub file_log_enabled: bool,
    /// Empty = `<profile>/data/logs`.
    pub file_log_path: String,
    pub file_log_backup_enabled: bool,
    /// KiB.
    pub file_log_max_size: i64,
    pub file_log_delete_old: bool,
    pub file_log_age: i64,
    pub file_log_age_type: FileLogAgeType,
    /// "Also remove the content files" default in the removal dialog.
    pub delete_torrent_content_files: bool,
}

impl Default for BehaviorPrefs {
    fn default() -> Self {
        Self {
            locale: String::new(),
            performance_warning: false,
            status_bar_external_ip: false,
            confirm_torrent_deletion: true,
            file_log_enabled: true,
            file_log_path: String::new(),
            file_log_backup_enabled: true,
            file_log_max_size: 65,
            file_log_delete_old: true,
            file_log_age: 1,
            file_log_age_type: FileLogAgeType::Months,
            delete_torrent_content_files: false,
        }
    }
}

/// A watched ("automatically add torrents from") folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct WatchedFolder {
    pub path: String,
    pub recursive: bool,
    pub params: TorrentParamsTemplate,
}

/// Downloads page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct DownloadsPrefs {
    pub torrent_content_layout: ContentLayout,
    pub add_to_top_of_queue: bool,
    pub add_stopped_enabled: bool,
    pub torrent_stop_condition: StopCondition,
    pub merge_trackers: bool,
    pub auto_delete_mode: AutoDeleteMode,
    pub preallocate_all: bool,
    /// Append `.!qB` to incomplete files.
    pub incomplete_files_ext: bool,
    pub use_unwanted_folder: bool,
    /// Default torrent management mode is Automatic.
    pub auto_tmm_enabled: bool,
    /// When category changed: relocate (true) / switch to manual (false).
    pub torrent_changed_tmm_enabled: bool,
    pub save_path_changed_tmm_enabled: bool,
    pub category_changed_tmm_enabled: bool,
    /// Subcategories ("A/B") are shown as a tree and inherit paths.
    pub use_subcategories: bool,
    /// Empty = `~/Downloads`.
    pub save_path: String,
    pub temp_path_enabled: bool,
    /// Empty = `<save_path>/temp`.
    pub temp_path: String,
    pub use_category_paths_in_manual_mode: bool,
    /// Copy .torrent files to ("" = off).
    pub export_dir: String,
    /// Copy .torrent files of finished downloads to ("" = off).
    pub export_dir_fin: String,
    pub watched_folders: Vec<WatchedFolder>,
    pub excluded_file_names_enabled: bool,
    pub excluded_file_names: Vec<String>,
    pub mail_notification_enabled: bool,
    pub mail_notification_sender: String,
    pub mail_notification_email: String,
    pub mail_notification_smtp: String,
    pub mail_notification_ssl_enabled: bool,
    pub mail_notification_auth_enabled: bool,
    pub mail_notification_username: String,
    pub mail_notification_password: String,
    pub autorun_on_torrent_added_enabled: bool,
    pub autorun_on_torrent_added_program: String,
    pub autorun_enabled: bool,
    pub autorun_program: String,
}

impl Default for DownloadsPrefs {
    fn default() -> Self {
        Self {
            torrent_content_layout: ContentLayout::Original,
            add_to_top_of_queue: false,
            add_stopped_enabled: false,
            torrent_stop_condition: StopCondition::None,
            merge_trackers: false,
            auto_delete_mode: AutoDeleteMode::Never,
            preallocate_all: false,
            incomplete_files_ext: false,
            use_unwanted_folder: false,
            auto_tmm_enabled: false,
            torrent_changed_tmm_enabled: true,
            save_path_changed_tmm_enabled: false,
            category_changed_tmm_enabled: false,
            use_subcategories: false,
            save_path: String::new(),
            temp_path_enabled: false,
            temp_path: String::new(),
            use_category_paths_in_manual_mode: false,
            export_dir: String::new(),
            export_dir_fin: String::new(),
            watched_folders: Vec::new(),
            excluded_file_names_enabled: false,
            excluded_file_names: Vec::new(),
            mail_notification_enabled: false,
            mail_notification_sender: "qBittorrent_notification@example.com".into(),
            mail_notification_email: String::new(),
            mail_notification_smtp: "smtp.changeme.com".into(),
            mail_notification_ssl_enabled: false,
            mail_notification_auth_enabled: false,
            mail_notification_username: String::new(),
            mail_notification_password: String::new(),
            autorun_on_torrent_added_enabled: false,
            autorun_on_torrent_added_program: String::new(),
            autorun_enabled: false,
            autorun_program: String::new(),
        }
    }
}

/// Connection page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct ConnectionPrefs {
    pub bittorrent_protocol: BtProtocol,
    /// 0 = any (OS picks). A random port 1024..=65535 is chosen on first start.
    pub listen_port: i64,
    pub ssl_enabled: bool,
    pub ssl_listen_port: i64,
    /// UPnP / NAT-PMP port forwarding.
    pub upnp: bool,
    /// -1 = unlimited for the four limits below.
    pub max_connec: i64,
    pub max_connec_per_torrent: i64,
    pub max_uploads: i64,
    pub max_uploads_per_torrent: i64,
    pub i2p_enabled: bool,
    pub i2p_address: String,
    pub i2p_port: i64,
    pub i2p_mixed_mode: bool,
    pub i2p_inbound_quantity: i64,
    pub i2p_outbound_quantity: i64,
    pub i2p_inbound_length: i64,
    pub i2p_outbound_length: i64,
    pub proxy_type: ProxyType,
    pub proxy_ip: String,
    pub proxy_port: i64,
    pub proxy_auth_enabled: bool,
    pub proxy_username: String,
    pub proxy_password: String,
    pub proxy_hostname_lookup: bool,
    pub proxy_bittorrent: bool,
    pub proxy_peer_connections: bool,
    pub proxy_rss: bool,
    pub proxy_misc: bool,
    pub ip_filter_enabled: bool,
    pub ip_filter_path: String,
    pub ip_filter_trackers: bool,
    pub banned_ips: Vec<String>,
}

impl Default for ConnectionPrefs {
    fn default() -> Self {
        Self {
            bittorrent_protocol: BtProtocol::Both,
            listen_port: -1,
            ssl_enabled: false,
            ssl_listen_port: -1,
            upnp: true,
            max_connec: 500,
            max_connec_per_torrent: 100,
            max_uploads: 20,
            max_uploads_per_torrent: 4,
            i2p_enabled: false,
            i2p_address: "127.0.0.1".into(),
            i2p_port: 7656,
            i2p_mixed_mode: false,
            i2p_inbound_quantity: 3,
            i2p_outbound_quantity: 3,
            i2p_inbound_length: 3,
            i2p_outbound_length: 3,
            proxy_type: ProxyType::None,
            proxy_ip: String::new(),
            proxy_port: 8080,
            proxy_auth_enabled: false,
            proxy_username: String::new(),
            proxy_password: String::new(),
            proxy_hostname_lookup: true,
            proxy_bittorrent: false,
            proxy_peer_connections: false,
            proxy_rss: false,
            proxy_misc: false,
            ip_filter_enabled: false,
            ip_filter_path: String::new(),
            ip_filter_trackers: false,
            banned_ips: Vec::new(),
        }
    }
}

/// Speed page. Limits in bytes/s, 0 = unlimited.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct SpeedPrefs {
    pub dl_limit: i64,
    pub up_limit: i64,
    pub alt_dl_limit: i64,
    pub alt_up_limit: i64,
    pub limit_utp_rate: bool,
    pub limit_tcp_overhead: bool,
    pub limit_lan_peers: bool,
    pub scheduler_enabled: bool,
    pub schedule_from_hour: i64,
    pub schedule_from_min: i64,
    pub schedule_to_hour: i64,
    pub schedule_to_min: i64,
    pub scheduler_days: SchedulerDays,
}

impl Default for SpeedPrefs {
    fn default() -> Self {
        Self {
            dl_limit: 0,
            up_limit: 0,
            alt_dl_limit: 10240,
            alt_up_limit: 10240,
            limit_utp_rate: true,
            limit_tcp_overhead: false,
            limit_lan_peers: true,
            scheduler_enabled: false,
            schedule_from_hour: 8,
            schedule_from_min: 0,
            schedule_to_hour: 20,
            schedule_to_min: 0,
            scheduler_days: SchedulerDays::EveryDay,
        }
    }
}

/// BitTorrent page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct BitTorrentPrefs {
    pub dht: bool,
    pub pex: bool,
    pub lsd: bool,
    pub encryption: EncryptionMode,
    pub anonymous_mode: bool,
    /// -1 = unlimited.
    pub max_active_checking_torrents: i64,
    pub queueing_enabled: bool,
    pub max_active_downloads: i64,
    pub max_active_torrents: i64,
    pub max_active_uploads: i64,
    pub dont_count_slow_torrents: bool,
    /// KiB/s.
    pub slow_torrent_dl_rate_threshold: i64,
    /// KiB/s.
    pub slow_torrent_ul_rate_threshold: i64,
    /// Seconds.
    pub slow_torrent_inactive_timer: i64,
    /// -1 = disabled.
    pub max_ratio: f64,
    /// Minutes, -1 = disabled.
    pub max_seeding_time: i64,
    /// Minutes, -1 = disabled.
    pub max_inactive_seeding_time: i64,
    /// Never `Default` at the global level.
    pub max_ratio_act: ShareLimitAction,
    pub add_trackers_enabled: bool,
    /// One URL per line, blank line = new tier.
    pub add_trackers: String,
    pub add_trackers_from_url_enabled: bool,
    pub add_trackers_url: String,
    /// Last fetched list (read-only).
    pub add_trackers_url_list: String,
}

impl Default for BitTorrentPrefs {
    fn default() -> Self {
        Self {
            dht: true,
            pex: true,
            lsd: true,
            encryption: EncryptionMode::Allow,
            anonymous_mode: false,
            max_active_checking_torrents: 1,
            queueing_enabled: false,
            max_active_downloads: 3,
            max_active_torrents: 5,
            max_active_uploads: 3,
            dont_count_slow_torrents: false,
            slow_torrent_dl_rate_threshold: 2,
            slow_torrent_ul_rate_threshold: 2,
            slow_torrent_inactive_timer: 60,
            max_ratio: -1.0,
            max_seeding_time: -1,
            max_inactive_seeding_time: -1,
            max_ratio_act: ShareLimitAction::Stop,
            add_trackers_enabled: false,
            add_trackers: String::new(),
            add_trackers_from_url_enabled: false,
            add_trackers_url: String::new(),
            add_trackers_url_list: String::new(),
        }
    }
}

/// RSS page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct RssPrefs {
    /// Minutes.
    pub rss_refresh_interval: i64,
    /// Seconds between requests to the same host.
    pub rss_fetch_delay: i64,
    pub rss_max_articles_per_feed: i64,
    pub rss_processing_enabled: bool,
    pub rss_auto_downloading_enabled: bool,
    pub rss_download_repack_proper_episodes: bool,
    pub rss_smart_episode_filters: Vec<String>,
}

impl Default for RssPrefs {
    fn default() -> Self {
        Self {
            rss_refresh_interval: 30,
            rss_fetch_delay: 2,
            rss_max_articles_per_feed: 50,
            rss_processing_enabled: false,
            rss_auto_downloading_enabled: false,
            rss_download_repack_proper_episodes: true,
            rss_smart_episode_filters: vec![
                r"s(\d+)e(\d+)".into(),
                r"(\d+)x(\d+)".into(),
                r"(\d{4}[.\-]\d{1,2}[.\-]\d{1,2})".into(),
                r"(\d{1,2}[.\-]\d{1,2}[.\-]\d{4})".into(),
            ],
        }
    }
}

/// WebUI page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct WebUiPrefs {
    pub web_ui_enabled: bool,
    /// `;`-separated, `*` wildcard.
    pub web_ui_domain_list: String,
    pub web_ui_address: String,
    pub web_ui_port: i64,
    pub web_ui_upnp: bool,
    pub use_https: bool,
    pub web_ui_https_cert_path: String,
    pub web_ui_https_key_path: String,
    pub web_ui_username: String,
    /// PBKDF2-HMAC-SHA512 hash, qBittorrent format `@ByteArray(salt:hash)`
    /// base64 parts; empty = no password (a temporary one is generated).
    pub web_ui_password_pbkdf2: String,
    /// `qbt_` + 28 chars, empty = none.
    pub web_ui_api_key: String,
    pub bypass_local_auth: bool,
    pub bypass_auth_subnet_whitelist_enabled: bool,
    pub bypass_auth_subnet_whitelist: Vec<String>,
    /// 0 = never ban.
    pub web_ui_max_auth_fail_count: i64,
    /// Seconds.
    pub web_ui_ban_duration: i64,
    /// Seconds, 0 = disabled.
    pub web_ui_session_timeout: i64,
    pub alternative_webui_enabled: bool,
    pub alternative_webui_path: String,
    pub web_ui_clickjacking_protection_enabled: bool,
    pub web_ui_csrf_protection_enabled: bool,
    pub web_ui_secure_cookie_enabled: bool,
    pub web_ui_host_header_validation_enabled: bool,
    pub web_ui_use_custom_http_headers_enabled: bool,
    /// `Header: value` per line.
    pub web_ui_custom_http_headers: String,
    pub web_ui_reverse_proxy_enabled: bool,
    /// `;`-separated IPs/subnets.
    pub web_ui_reverse_proxies_list: String,
    pub dyndns_enabled: bool,
    pub dyndns_service: DynDnsService,
    pub dyndns_username: String,
    pub dyndns_password: String,
    pub dyndns_domain: String,
}

impl Default for WebUiPrefs {
    fn default() -> Self {
        Self {
            web_ui_enabled: false,
            web_ui_domain_list: "*".into(),
            web_ui_address: "*".into(),
            web_ui_port: 8080,
            web_ui_upnp: false,
            use_https: false,
            web_ui_https_cert_path: String::new(),
            web_ui_https_key_path: String::new(),
            web_ui_username: "admin".into(),
            web_ui_password_pbkdf2: String::new(),
            web_ui_api_key: String::new(),
            bypass_local_auth: false,
            bypass_auth_subnet_whitelist_enabled: false,
            bypass_auth_subnet_whitelist: Vec::new(),
            web_ui_max_auth_fail_count: 5,
            web_ui_ban_duration: 3600,
            web_ui_session_timeout: 3600,
            alternative_webui_enabled: false,
            alternative_webui_path: String::new(),
            web_ui_clickjacking_protection_enabled: true,
            web_ui_csrf_protection_enabled: true,
            web_ui_secure_cookie_enabled: true,
            web_ui_host_header_validation_enabled: true,
            web_ui_use_custom_http_headers_enabled: false,
            web_ui_custom_http_headers: String::new(),
            web_ui_reverse_proxy_enabled: false,
            web_ui_reverse_proxies_list: String::new(),
            dyndns_enabled: false,
            dyndns_service: DynDnsService::DynDns,
            dyndns_username: String::new(),
            dyndns_password: String::new(),
            dyndns_domain: "changeme.dyndns.org".into(),
        }
    }
}

/// Advanced page ("qBittorrent Section" + "libtorrent Section"). Engine
/// tuning knobs are mapped to their closest equivalent in the Rust engine;
/// see docs/ENGINE.md for which ones have an effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(default)]
pub struct AdvancedPrefs {
    pub resume_data_storage_type: ResumeDataStorageType,
    pub torrent_content_remove_option: ContentRemoveOption,
    /// MiB.
    pub memory_working_set_limit: i64,
    pub current_network_interface: String,
    pub current_interface_name: String,
    pub current_interface_address: String,
    /// Minutes, 0 = disabled.
    pub save_resume_data_interval: i64,
    /// Minutes, 0 = disabled.
    pub save_statistics_interval: i64,
    /// Bytes.
    pub torrent_file_size_limit: i64,
    pub confirm_torrent_recheck: bool,
    pub recheck_completed_torrents: bool,
    pub app_instance_name: String,
    /// Milliseconds.
    pub refresh_interval: i64,
    pub resolve_peer_host_names: bool,
    pub resolve_peer_countries: bool,
    pub reannounce_when_address_changed: bool,
    pub enable_embedded_tracker: bool,
    pub embedded_tracker_port: i64,
    pub embedded_tracker_port_forwarding: bool,
    /// macOS: set `com.apple.quarantine` on downloaded files.
    pub mark_of_the_web: bool,
    pub ignore_ssl_errors: bool,
    pub python_executable_path: String,
    /// GUI-only in qBittorrent: start the session paused.
    pub start_paused: bool,
    /// GUI-only: session shutdown timeout in seconds, -1 = unlimited.
    pub shutdown_timeout: i64,
    pub bdecode_depth_limit: i64,
    pub bdecode_token_limit: i64,
    pub async_io_threads: i64,
    pub hashing_threads: i64,
    pub file_pool_size: i64,
    /// MiB.
    pub checking_memory_use: i64,
    /// MiB, -1 = auto.
    pub disk_cache: i64,
    /// Seconds.
    pub disk_cache_ttl: i64,
    /// Bytes.
    pub disk_queue_size: i64,
    pub disk_io_type: DiskIoType,
    pub disk_io_read_mode: DiskIoReadMode,
    pub disk_io_write_mode: DiskIoWriteMode,
    pub enable_coalesce_read_write: bool,
    pub enable_piece_extent_affinity: bool,
    pub enable_upload_suggestions: bool,
    /// KiB.
    pub send_buffer_watermark: i64,
    /// KiB.
    pub send_buffer_low_watermark: i64,
    /// Percent.
    pub send_buffer_watermark_factor: i64,
    /// Outgoing connections per second.
    pub connection_speed: i64,
    /// Bytes, 0 = system default.
    pub socket_send_buffer_size: i64,
    pub socket_receive_buffer_size: i64,
    pub socket_backlog_size: i64,
    pub outgoing_ports_min: i64,
    pub outgoing_ports_max: i64,
    /// Seconds, 0 = permanent.
    pub upnp_lease_duration: i64,
    pub peer_tos: i64,
    pub utp_tcp_mixed_mode: UtpMixedMode,
    pub hostname_cache_ttl: i64,
    pub idn_support_enabled: bool,
    pub enable_multi_connections_from_same_ip: bool,
    pub validate_https_tracker_certificate: bool,
    pub ssrf_mitigation: bool,
    pub block_peers_on_privileged_ports: bool,
    pub upload_slots_behavior: UploadSlotsBehavior,
    pub upload_choking_algorithm: UploadChokingAlgorithm,
    pub announce_to_all_trackers: bool,
    pub announce_to_all_tiers: bool,
    pub announce_ip: String,
    pub announce_port: i64,
    pub max_concurrent_http_announces: i64,
    /// Seconds, 0 = disabled.
    pub stop_tracker_timeout: i64,
    /// Percent.
    pub peer_turnover: i64,
    /// Percent.
    pub peer_turnover_cutoff: i64,
    /// Seconds.
    pub peer_turnover_interval: i64,
    pub request_queue_size: i64,
    /// Comma-separated `host:port`.
    pub dht_bootstrap_nodes: String,
}

pub const DEFAULT_DHT_BOOTSTRAP_NODES: &str =
    "dht.libtorrent.org:25401, dht.transmissionbt.com:6881, router.bittorrent.com:6881";

impl Default for AdvancedPrefs {
    fn default() -> Self {
        Self {
            resume_data_storage_type: ResumeDataStorageType::Legacy,
            torrent_content_remove_option: ContentRemoveOption::Delete,
            memory_working_set_limit: 512,
            current_network_interface: String::new(),
            current_interface_name: String::new(),
            current_interface_address: String::new(),
            save_resume_data_interval: 60,
            save_statistics_interval: 15,
            torrent_file_size_limit: 100 * 1024 * 1024,
            confirm_torrent_recheck: true,
            recheck_completed_torrents: false,
            app_instance_name: String::new(),
            refresh_interval: 1500,
            resolve_peer_host_names: false,
            resolve_peer_countries: true,
            reannounce_when_address_changed: false,
            enable_embedded_tracker: false,
            embedded_tracker_port: 9000,
            embedded_tracker_port_forwarding: false,
            mark_of_the_web: true,
            ignore_ssl_errors: false,
            python_executable_path: String::new(),
            start_paused: false,
            shutdown_timeout: -1,
            bdecode_depth_limit: 100,
            bdecode_token_limit: 10_000_000,
            async_io_threads: 10,
            hashing_threads: 1,
            file_pool_size: 100,
            checking_memory_use: 32,
            disk_cache: -1,
            disk_cache_ttl: 60,
            disk_queue_size: 1024 * 1024,
            disk_io_type: DiskIoType::Default,
            disk_io_read_mode: DiskIoReadMode::EnableOsCache,
            disk_io_write_mode: DiskIoWriteMode::EnableOsCache,
            enable_coalesce_read_write: false,
            enable_piece_extent_affinity: false,
            enable_upload_suggestions: false,
            send_buffer_watermark: 500,
            send_buffer_low_watermark: 10,
            send_buffer_watermark_factor: 50,
            connection_speed: 30,
            socket_send_buffer_size: 0,
            socket_receive_buffer_size: 0,
            socket_backlog_size: 30,
            outgoing_ports_min: 0,
            outgoing_ports_max: 0,
            upnp_lease_duration: 0,
            peer_tos: 1,
            utp_tcp_mixed_mode: UtpMixedMode::PreferTcp,
            hostname_cache_ttl: 1200,
            idn_support_enabled: false,
            enable_multi_connections_from_same_ip: false,
            validate_https_tracker_certificate: true,
            ssrf_mitigation: true,
            block_peers_on_privileged_ports: false,
            upload_slots_behavior: UploadSlotsBehavior::FixedSlots,
            upload_choking_algorithm: UploadChokingAlgorithm::FastestUpload,
            announce_to_all_trackers: false,
            announce_to_all_tiers: true,
            announce_ip: String::new(),
            announce_port: 0,
            max_concurrent_http_announces: 50,
            stop_tracker_timeout: 2,
            peer_turnover: 4,
            peer_turnover_cutoff: 90,
            peer_turnover_interval: 300,
            request_queue_size: 500,
            dht_bootstrap_nodes: DEFAULT_DHT_BOOTSTRAP_NODES.into(),
        }
    }
}
