//! Thin adapter over the librqbit engine fork. Every engine call made by
//! the session goes through this file. Remaining gaps of the engine API are
//! marked with `ENGINE:` comments together with the fallback in use.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use librqbit::api::TorrentIdOrHash;
use librqbit::dht::DhtPersistenceConfig;
use librqbit::limits::LimitsConfig;
use librqbit::storage::{DiskCacheMode, MoveMode, StorageSettings, TorrentErrorKind};
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, ChokingAlgorithm, ConnectionKind, DeleteOptions, DhtSessionConfig,
    DisabledDiscovery,
    EncryptionPolicy, I2pSettings, ListenerMode, ListenerOptions, MappingProtocol, ManagedTorrent, ManagedTorrentState,
    MixedModeAlgorithm, NetSettings, PeerPolicyOptions, PeerSource, PortMappingConfig, ProxyConfig, ProxyKind, ProxySettings,
    SeedChokingAlgorithm, SessionOptions, SessionPersistenceConfig, TrackerSettings, TrackerState,
};
use parking_lot::Mutex;

use crate::prefs::{
    BtProtocol, DiskIoReadMode, DiskIoWriteMode, EncryptionMode, Preferences, ProxyType, UploadChokingAlgorithm,
    UploadSlotsBehavior, UtpMixedMode,
};

pub type EngineHandle = Arc<ManagedTorrent>;

/// Settings that still require a new engine session when they change.
#[derive(Clone, Debug, PartialEq)]
pub struct EngineConfig {
    /// DHT was started with the session (its bootstrap configuration is
    /// fixed at start; the engine can toggle it but not reconfigure it).
    pub dht: bool,
    pub bootstrap_nodes: Vec<String>,
    pub engine_dir: PathBuf,
    pub client_name: String,
}

impl EngineConfig {
    /// ENGINE: everything networking-related is applied live (see
    /// `NetPrefs`). Only the DHT bootstrap configuration is fixed when the
    /// session is created: a changed node list, or enabling DHT on a session
    /// that started without it, starts a new session (torrents are re-added;
    /// fast-resume bitfields avoid rechecks). `self` is the wanted config.
    pub fn restart_required(&self, running: &EngineConfig) -> bool {
        (running.dht && self.bootstrap_nodes != running.bootstrap_nodes)
            || (self.dht && !running.dht)
            || self.engine_dir != running.engine_dir
            || self.client_name != running.client_name
    }
}

/// Networking preferences applied to the engine live (on start and on every
/// preference change).
#[derive(Clone, Debug, PartialEq)]
pub struct NetPrefs {
    pub net: NetSettings,
    pub listen_port: u16,
    pub protocol: BtProtocol,
    pub listen_ip: Option<IpAddr>,
    pub interface: Option<String>,
    pub dht: bool,
    pub lsd: bool,
    pub port_mapping: PortMappingConfig,
    /// Additional ports to map next to the listen port (WebUI, tracker).
    pub extra_ports: Vec<(MappingProtocol, u16)>,
    pub i2p: Option<I2pSettings>,
}

impl NetPrefs {
    pub fn from_prefs(p: &Preferences) -> Self {
        let a = &p.advanced;
        let c = &p.connection;
        let (enable_tcp, enable_utp) = match c.bittorrent_protocol {
            BtProtocol::Both => (true, true),
            BtProtocol::Tcp => (true, false),
            BtProtocol::Utp => (false, true),
        };
        let proxy = (c.proxy_type != ProxyType::None && !c.proxy_ip.trim().is_empty()).then(|| {
            let auth = (c.proxy_auth_enabled && !c.proxy_username.is_empty()).then(|| (c.proxy_username.clone(), c.proxy_password.clone()));
            ProxySettings {
                config: ProxyConfig {
                    kind: match c.proxy_type {
                        ProxyType::Http => ProxyKind::Http,
                        ProxyType::Socks4 => ProxyKind::Socks4,
                        _ => ProxyKind::Socks5,
                    },
                    host: c.proxy_ip.trim().to_string(),
                    port: c.proxy_port.clamp(1, 65535) as u16,
                    auth,
                    remote_dns: c.proxy_hostname_lookup,
                },
                // "Use proxy for BitTorrent purposes" routes trackers; peers
                // follow only with "Use proxy for peer connections".
                peer_connections: c.proxy_bittorrent && c.proxy_peer_connections,
                tracker_connections: c.proxy_bittorrent,
            }
        });
        let net = NetSettings {
            encryption: match p.bittorrent.encryption {
                EncryptionMode::Allow => EncryptionPolicy::Enabled,
                EncryptionMode::Require => EncryptionPolicy::Forced,
                EncryptionMode::Disable => EncryptionPolicy::Disabled,
            },
            proxy: proxy.filter(|_| c.proxy_bittorrent),
            anonymous_mode: p.bittorrent.anonymous_mode,
            enable_tcp,
            enable_utp,
            outgoing_ports: (a.outgoing_ports_min > 0 && a.outgoing_ports_max >= a.outgoing_ports_min)
                .then(|| (a.outgoing_ports_min.min(65535) as u16, a.outgoing_ports_max.min(65535) as u16)),
            peer_tos: a.peer_tos.clamp(0, 255) as u8,
            send_buffer_size: a.socket_send_buffer_size.clamp(0, u32::MAX as i64) as u32,
            recv_buffer_size: a.socket_receive_buffer_size.clamp(0, u32::MAX as i64) as u32,
            connection_speed: a.connection_speed.clamp(0, u32::MAX as i64) as u32,
            pex: p.bittorrent.pex,
        };
        let mut extra_ports = Vec::new();
        if p.webui.web_ui_enabled && p.webui.web_ui_upnp {
            extra_ports.push((MappingProtocol::Tcp, p.webui.web_ui_port.clamp(1, 65535) as u16));
        }
        if a.enable_embedded_tracker && a.embedded_tracker_port_forwarding {
            extra_ports.push((MappingProtocol::Tcp, a.embedded_tracker_port.clamp(1, 65535) as u16));
        }
        extra_ports.sort();
        extra_ports.dedup();
        let i2p = c.i2p_enabled.then(|| I2pSettings {
            sam: librqbit::SamOptions {
                host: if c.i2p_address.trim().is_empty() { "127.0.0.1".into() } else { c.i2p_address.trim().to_string() },
                port: c.i2p_port.clamp(1, 65535) as u16,
                inbound_quantity: c.i2p_inbound_quantity.clamp(1, 16) as u8,
                outbound_quantity: c.i2p_outbound_quantity.clamp(1, 16) as u8,
                inbound_length: c.i2p_inbound_length.clamp(0, 7) as u8,
                outbound_length: c.i2p_outbound_length.clamp(0, 7) as u8,
            },
            mixed_mode: c.i2p_mixed_mode,
        });
        Self {
            net,
            listen_port: c.listen_port.clamp(0, 65535) as u16,
            protocol: c.bittorrent_protocol,
            listen_ip: a.current_interface_address.trim().parse().ok(),
            interface: Some(a.current_network_interface.trim().to_string()).filter(|s| !s.is_empty()),
            dht: p.bittorrent.dht,
            lsd: p.bittorrent.lsd,
            port_mapping: PortMappingConfig {
                upnp: c.upnp,
                natpmp: c.upnp,
                natpmp_gateway: None,
                lease_secs: a.upnp_lease_duration.clamp(0, u32::MAX as i64) as u32,
                description: Some(super::policy::client_name()),
            },
            extra_ports,
            i2p,
        }
    }

    fn listener(&self, base: Option<ListenerOptions>) -> ListenerOptions {
        let mut o = base.unwrap_or_default();
        o.mode = match self.protocol {
            BtProtocol::Both => ListenerMode::TcpAndUtp,
            BtProtocol::Tcp => ListenerMode::TcpOnly,
            BtProtocol::Utp => ListenerMode::UtpOnly,
        };
        let ip = self.listen_ip.unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        o.listen_addr = SocketAddr::new(ip, self.listen_port);
        o.enable_upnp_port_forwarding = self.port_mapping.upnp;
        o
    }

    fn listen_key(&self) -> (u16, BtProtocol, Option<IpAddr>, bool) {
        (self.listen_port, self.protocol, self.listen_ip, self.port_mapping.upnp)
    }
}

/// Outcome of applying `NetPrefs` to a running session.
#[derive(Debug, Default)]
pub struct NetReport {
    /// The listener was (re)bound: `Ok(address)` or the error.
    pub listen: Option<Result<SocketAddr, String>>,
    /// Other failures (user-facing messages).
    pub errors: Vec<String>,
    /// The proxy or the network interface could not be applied: the session
    /// keeps the previous ones (e.g. direct connections), so it must be
    /// restarted, which fails closed (no session) if the problem persists.
    pub fail_closed: bool,
}

/// Settings applied live (on start and on every preference change).
pub struct LiveSettings {
    pub storage: StorageSettings,
    pub checking_limit: i64,
    pub peer_policy: PeerPolicyOptions,
    pub trackers: TrackerSettings,
    pub net: NetPrefs,
}

fn limit_opt(v: i64) -> Option<u32> {
    (v > 0).then(|| v.min(u32::MAX as i64) as u32)
}

impl LiveSettings {
    pub fn from_prefs(p: &Preferences) -> Self {
        let a = &p.advanced;
        let c = &p.connection;
        let storage = StorageSettings {
            incomplete_suffix: p.downloads.incomplete_files_ext.then(|| super::paths::QB_EXT.to_string()),
            use_unwanted_folder: p.downloads.use_unwanted_folder,
            preallocate: p.downloads.preallocate_all,
            file_pool_size: a.file_pool_size.clamp(1, 100_000) as usize,
            read_cache: match a.disk_io_read_mode {
                DiskIoReadMode::DisableOsCache => DiskCacheMode::Disabled,
                DiskIoReadMode::EnableOsCache => DiskCacheMode::Enabled,
            },
            write_cache: match a.disk_io_write_mode {
                DiskIoWriteMode::DisableOsCache => DiskCacheMode::Disabled,
                DiskIoWriteMode::EnableOsCache => DiskCacheMode::Enabled,
                DiskIoWriteMode::WriteThrough => DiskCacheMode::WriteThrough,
            },
        };
        let peer_policy = PeerPolicyOptions {
            max_uploads: limit_opt(c.max_uploads),
            max_uploads_per_torrent: limit_opt(c.max_uploads_per_torrent),
            choking_algorithm: match a.upload_slots_behavior {
                UploadSlotsBehavior::FixedSlots => ChokingAlgorithm::FixedSlots,
                UploadSlotsBehavior::RateBased => ChokingAlgorithm::RateBased,
            },
            seed_choking_algorithm: match a.upload_choking_algorithm {
                UploadChokingAlgorithm::RoundRobin => SeedChokingAlgorithm::RoundRobin,
                UploadChokingAlgorithm::FastestUpload => SeedChokingAlgorithm::FastestUpload,
                UploadChokingAlgorithm::AntiLeech => SeedChokingAlgorithm::AntiLeech,
            },
            max_connections: limit_opt(c.max_connec),
            rate_limit_utp: p.speed.limit_utp_rate,
            rate_limit_lan: p.speed.limit_lan_peers,
            mixed_mode: match a.utp_tcp_mixed_mode {
                UtpMixedMode::PreferTcp => MixedModeAlgorithm::PreferTcp,
                UtpMixedMode::PeerProportional => MixedModeAlgorithm::PeerProportional,
            },
        };
        let trackers = TrackerSettings {
            announce_ip: a.announce_ip.trim().parse().ok(),
            announce_port: a.announce_port.clamp(0, 65535) as u16,
            max_concurrent_http_announces: a.max_concurrent_http_announces.max(0) as usize,
            validate_https_tracker_certificate: a.validate_https_tracker_certificate,
            ssrf_mitigation: a.ssrf_mitigation,
            stop_tracker_timeout: Duration::from_secs(a.stop_tracker_timeout.max(0) as u64),
            announce_to_all_trackers: a.announce_to_all_trackers,
            announce_to_all_tiers: a.announce_to_all_tiers,
            anonymous_mode: p.bittorrent.anonymous_mode,
            ..TrackerSettings::default()
        };
        Self {
            storage,
            checking_limit: p.bittorrent.max_active_checking_torrents,
            peer_policy,
            trackers,
            net: NetPrefs::from_prefs(p),
        }
    }
}

/// Per-torrent values handed over at add time and changeable later.
#[derive(Clone, Debug)]
pub struct TorrentSettings {
    pub priorities: Vec<u8>,
    pub sequential: bool,
    pub first_last: bool,
    pub super_seeding: bool,
    pub download_limit: u64,
    pub upload_limit: u64,
    pub peer_limit: Option<usize>,
    pub max_uploads: Option<u32>,
    /// Per-torrent DHT/PeX/LSD switches.
    pub disabled_discovery: DisabledDiscovery,
}

pub struct AddRequest {
    pub torrent_bytes: Vec<u8>,
    pub paused: bool,
    pub output_folder: PathBuf,
    /// Relative path per engine file index (content layout and renames).
    pub renamed_files: BTreeMap<usize, PathBuf>,
    pub trackers: Vec<(String, u32)>,
    /// Exactly these web seeds (BEP 19; the core's persisted list).
    pub web_seeds: Vec<String>,
    /// Trackers and web seeds trusted by SSRF mitigation (all others are
    /// treated as coming from a remote source).
    pub trusted_urls: Vec<String>,
    pub initial_peers: Vec<SocketAddr>,
    pub skip_hash_check: bool,
    pub settings: TorrentSettings,
}

pub struct ResolvedMetadata {
    pub torrent_bytes: Vec<u8>,
    pub seen_peers: Vec<SocketAddr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    MissingFiles,
    DiskFull,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EnginePhase {
    /// Hash check (or waiting for a check slot).
    Initializing { checked_bytes: u64 },
    Paused,
    Live,
    Error(String, ErrorKind),
}

#[derive(Clone, Debug)]
pub struct EngineStatus {
    pub phase: EnginePhase,
    /// Have-bytes per engine file index (empty while initializing).
    pub file_progress: Vec<u64>,
    pub finished: bool,
    pub dl_rate: i64,
    pub ul_rate: i64,
    /// Counters of the current live period (reset when paused).
    pub fetched: u64,
    pub uploaded: u64,
    pub peers_live: u32,
    pub seeders_live: u32,
    pub moving: bool,
    pub wasted: u64,
    pub connections: u32,
}

#[derive(Clone, Debug, Default)]
pub struct TrackerSummary {
    pub statuses: Vec<librqbit::TrackerStatus>,
    pub swarm_seeds: Option<u32>,
    pub swarm_leechers: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EngineTransferStats {
    pub payload_downloaded: u64,
    pub payload_uploaded: u64,
    pub total_downloaded: u64,
    pub total_uploaded: u64,
    pub wasted: u64,
    pub payload_dl_rate: i64,
    pub payload_ul_rate: i64,
    pub peers: u32,
    pub dht_nodes: i64,
}

pub struct Engine {
    session: Arc<librqbit::Session>,
    pub cfg: EngineConfig,
    pub generation: u64,
    /// The networking preferences last applied to the session.
    applied: Mutex<NetPrefs>,
}

/// Drops repeated reports of the same address within a second.
#[derive(Default)]
struct RepeatFilter {
    last: std::collections::HashMap<IpAddr, std::time::Instant>,
}

impl RepeatFilter {
    const WINDOW: Duration = Duration::from_secs(1);
    const MAX_TRACKED: usize = 4096;

    fn should_log(&mut self, ip: IpAddr, now: std::time::Instant) -> bool {
        if let Some(t) = self.last.get(&ip)
            && now.saturating_duration_since(*t) < Self::WINDOW
        {
            return false;
        }
        if self.last.len() >= Self::MAX_TRACKED {
            self.last.retain(|_, t| now.saturating_duration_since(*t) < Self::WINDOW);
            if self.last.len() >= Self::MAX_TRACKED {
                self.last.clear();
            }
        }
        self.last.insert(ip, now);
        true
    }
}

fn nz(v: u64) -> Option<NonZeroU32> {
    NonZeroU32::new(v.min(u32::MAX as u64) as u32)
}

impl Engine {
    /// Starts an engine session. If listening fails, the session is started
    /// without a listener (status "disconnected") and the error is returned
    /// alongside.
    pub async fn start(cfg: EngineConfig, live: LiveSettings, generation: u64) -> anyhow::Result<(Self, Option<String>)> {
        let _ = librqbit::try_increase_nofile_limit();
        let resume_dir = cfg.engine_dir.join("fastresume");
        std::fs::create_dir_all(&resume_dir)?;
        // The engine's store is used only for its fast-resume bitfields
        // (`<hash>.bitv`): the core re-adds torrents itself, so the engine
        // must not restore them on its own. Older versions also kept a
        // session database and a copy of every .torrent there.
        if let Ok(rd) = std::fs::read_dir(&resume_dir) {
            for entry in rd.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name == "session.json" || name == "session.json.tmp" || name.ends_with(".torrent") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }

        let net = live.net.clone();
        let build = |with_listener: bool| SessionOptions {
            // Without DHT at start the session keeps no DHT configuration;
            // enabling it later restarts the session (see `restart_required`).
            dht: cfg.dht.then(|| DhtSessionConfig {
                bootstrap_addrs: (!cfg.bootstrap_nodes.is_empty()).then(|| cfg.bootstrap_nodes.clone()),
                port: None,
                persistence: Some(DhtPersistenceConfig {
                    dump_interval: None,
                    config_filename: Some(cfg.engine_dir.join("dht.json")),
                }),
            }),
            bind_device_name: net.interface.clone(),
            fastresume: true,
            persistence: Some(SessionPersistenceConfig::FastResumeOnly { folder: resume_dir.clone() }),
            peer_id: Some(librqbit::generate_azereus_style(*b"NR", (0, 2, 2, 0))),
            listen: with_listener.then(|| net.listener(None)),
            net: net.net.clone(),
            port_mapping: Some(net.port_mapping.clone()),
            i2p: net.i2p.clone(),
            concurrent_init_limit: Some(if live.checking_limit < 0 { 64 } else { live.checking_limit.clamp(1, 64) as usize }),
            storage: live.storage.clone(),
            disable_local_service_discovery: !net.lsd,
            client_name_and_version: Some(cfg.client_name.clone()),
            peer_policy: live.peer_policy,
            tracker_settings: live.trackers.clone(),
            ..Default::default()
        };

        let default_dir = cfg.engine_dir.join("default");
        let (session, err) = match librqbit::Session::new_with_opts(default_dir.clone(), build(true)).await {
            Ok(s) => (s, None),
            Err(e) => (librqbit::Session::new_with_opts(default_dir, build(false)).await?, Some(format!("{e:#}"))),
        };
        session.set_max_active_checking_torrents(live.checking_limit);
        for (proto, port) in &net.extra_ports {
            session.add_port_mapping(*proto, *port);
        }
        let applied = Mutex::new(net);
        Ok((Self { session, cfg, generation, applied }, err))
    }

    pub async fn stop(&self) {
        self.session.stop().await;
    }

    /// Applies the live settings to the running session.
    pub async fn apply_live(&self, live: &LiveSettings) -> NetReport {
        if self.session.storage_settings() != live.storage {
            let _ = self.session.set_storage_settings(live.storage.clone()).await;
        }
        self.session.set_max_active_checking_torrents(live.checking_limit);
        let p = live.peer_policy;
        self.session.set_max_uploads(p.max_uploads);
        self.session.set_default_max_uploads_per_torrent(p.max_uploads_per_torrent);
        self.session.set_choking_algorithm(p.choking_algorithm);
        self.session.set_seed_choking_algorithm(p.seed_choking_algorithm);
        self.session.set_max_connections(p.max_connections);
        self.session.set_rate_limit_utp(p.rate_limit_utp);
        self.session.set_rate_limit_lan(p.rate_limit_lan);
        self.session.set_mixed_mode_algorithm(p.mixed_mode);
        let _ = self.session.set_tracker_settings(live.trackers.clone());
        self.apply_net(&live.net).await
    }

    /// Applies the networking preferences, touching only what changed.
    async fn apply_net(&self, want: &NetPrefs) -> NetReport {
        let mut rep = NetReport::default();
        let have = self.applied.lock().clone();
        if &have == want {
            return rep;
        }
        let s = &self.session;
        if s.net_settings() != want.net
            && let Err(e) = s.set_net_settings(want.net.clone())
        {
            rep.errors.push(format!("Failed to apply the network settings. Reason: \"{e:#}\""));
            rep.fail_closed = true;
        }
        if have.interface != want.interface
            && let Err(e) = s.set_network_interface(want.interface.clone()).await
        {
            rep.errors.push(format!("Failed to bind to the network interface. Reason: \"{e:#}\""));
            rep.fail_closed = true;
        }
        if rep.fail_closed {
            // Leave `applied` as it was: nothing else is changed on a session
            // that is about to be replaced.
            return rep;
        }
        if have.listen_key() != want.listen_key() {
            let opts = want.listener(s.listen_options());
            rep.listen = Some(match s.set_listen_options(Some(opts)).await {
                Ok(()) => s.listen_addr().ok_or_else(|| "not listening".to_string()),
                Err(e) => Err(format!("{e:#}")),
            });
        }
        if s.dht_enabled() != want.dht
            && let Err(e) = s.set_dht_enabled(want.dht).await
        {
            rep.errors.push(format!("Failed to switch DHT. Reason: \"{e:#}\""));
        }
        if s.lsd_enabled() != want.lsd && !s.set_lsd_enabled(want.lsd).await && want.lsd {
            rep.errors.push("Failed to start Local Service Discovery.".to_string());
        }
        if s.port_mapping_config() != want.port_mapping {
            s.set_port_mapping(want.port_mapping.clone());
        }
        for e in have.extra_ports.iter().filter(|e| !want.extra_ports.contains(e)) {
            s.remove_port_mapping(e.0, e.1);
        }
        for e in want.extra_ports.iter().filter(|e| !have.extra_ports.contains(e)) {
            s.add_port_mapping(e.0, e.1);
        }
        if s.i2p_settings() != want.i2p {
            s.set_i2p(want.i2p.clone());
        }
        *self.applied.lock() = want.clone();
        rep
    }

    // ---- IP filter and bans -------------------------------------------------------

    /// Live peer IP filter (the file filter; manual bans are separate).
    pub fn set_peer_ip_filter(&self, filter: Option<Arc<crate::ipfilter::IpFilter>>) {
        self.session.set_peer_ip_filter(filter.map(|f| -> librqbit::IpFilterFn { Arc::new(move |ip| f.contains(ip)) }));
    }

    pub fn set_banned_ips(&self, ips: Vec<IpAddr>) {
        self.session.set_banned_ips(ips);
    }

    pub fn ban_ip(&self, ip: IpAddr) {
        self.session.ban_peer_ip(ip);
    }

    /// Feeds blocked connections into the "Blocked IPs" log.
    pub fn set_blocked_peer_logger(&self, logger: Arc<crate::logger::Logger>) {
        let repeats = Mutex::new(RepeatFilter::default());
        self.session.set_blocked_peer_callback(Some(Arc::new(move |b: librqbit::BlockedPeer| {
            // A host reconnecting in a loop must not push every other entry
            // out of the (bounded) log.
            if !repeats.lock().should_log(b.addr.ip(), std::time::Instant::now()) {
                return;
            }
            let reason = match b.reason {
                librqbit::PeerBlockReason::IpFilter | librqbit::PeerBlockReason::Banned => "IP filter",
                librqbit::PeerBlockReason::I2pMixedMode => "I2P mixed mode restrictions",
                _ => "blocked",
            };
            logger.peer(b.addr.ip().to_string(), true, reason);
        })));
    }

    // ---- Port mapping ---------------------------------------------------------------

    pub fn port_mapping_status(&self) -> librqbit::PortMappingStatus {
        self.session.port_mapping_status()
    }

    /// Our public IPv4 / IPv6 address, voted on by the port mapping gateway,
    /// trackers and peers (non-public addresses are never reported).
    pub fn external_ips(&self) -> (Option<std::net::IpAddr>, Option<std::net::IpAddr>) {
        self.session.external_ips()
    }

    /// "Apply to trackers": the IP filter also blocks tracker addresses.
    pub fn set_tracker_ip_filter(&self, filter: Option<Arc<crate::ipfilter::IpFilter>>) {
        self.session.set_tracker_ip_filter(filter.map(|f| -> librqbit::IpFilterFn { Arc::new(move |ip| f.contains(ip)) }));
    }

    pub fn is_listening(&self) -> bool {
        self.session.listen_addr().is_some()
    }

    /// Whether any incoming peer connection was accepted by this session.
    pub fn incoming_connection_seen(&self) -> bool {
        self.session.incoming_connection_seen()
    }

    pub fn listen_port(&self) -> Option<u16> {
        self.session.listen_addr().map(|a| a.port())
    }

    /// Global rate limits in bytes/s (0 = unlimited).
    pub fn set_rate_limits(&self, download: u64, upload: u64) {
        self.session.ratelimits.set_download_bps(nz(download));
        self.session.ratelimits.set_upload_bps(nz(upload));
    }

    pub async fn add(&self, req: AddRequest) -> anyhow::Result<EngineHandle> {
        let s = &req.settings;
        let wanted: Vec<usize> = s.priorities.iter().enumerate().filter(|(_, p)| **p > 0).map(|(i, _)| i).collect();
        let opts = AddTorrentOptions {
            paused: req.paused,
            only_files: (wanted.len() != s.priorities.len()).then_some(wanted),
            overwrite: true,
            output_folder: Some(req.output_folder.to_string_lossy().into_owned()),
            ratelimits: LimitsConfig { upload_bps: nz(s.upload_limit), download_bps: nz(s.download_limit) },
            initial_peers: (!req.initial_peers.is_empty()).then_some(req.initial_peers),
            peer_limit: s.peer_limit,
            replace_trackers: Some(req.trackers),
            replace_web_seeds: Some(req.web_seeds),
            trackers_local_source: Some(false),
            trusted_urls: Some(req.trusted_urls),
            skip_hash_check: req.skip_hash_check,
            renamed_files: Some(req.renamed_files),
            owns_output_folder: Some(false),
            disabled_discovery: s.disabled_discovery,
            ..Default::default()
        };
        let h = match self.session.add_torrent(AddTorrent::from_bytes(req.torrent_bytes), Some(opts)).await? {
            AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
            AddTorrentResponse::ListOnly(_) => bail!("unexpected engine response"),
        };
        self.apply_torrent_settings(&h, s);
        Ok(h)
    }

    pub fn apply_torrent_settings(&self, h: &EngineHandle, s: &TorrentSettings) {
        if !s.priorities.is_empty() && h.file_priorities() != s.priorities {
            let _ = h.set_file_priorities(s.priorities.clone());
        }
        h.set_sequential(s.sequential);
        h.set_first_last_piece_priority(s.first_last);
        if h.super_seeding() != s.super_seeding {
            h.set_super_seeding(s.super_seeding);
        }
        h.set_ratelimits(LimitsConfig { upload_bps: nz(s.upload_limit), download_bps: nz(s.download_limit) });
        h.set_peer_limit(s.peer_limit);
        h.set_max_uploads(s.max_uploads);
        if h.disabled_discovery() != s.disabled_discovery {
            h.set_disabled_discovery(s.disabled_discovery);
        }
    }

    /// Fetches the metadata of a magnet link from the swarm without adding
    /// the torrent (cancel by dropping the future). `disabled`: the
    /// torrent's DHT/PeX/LSD switches.
    pub async fn resolve_metadata(
        &self,
        magnet: &str,
        peers: Vec<SocketAddr>,
        disabled: DisabledDiscovery,
    ) -> anyhow::Result<ResolvedMetadata> {
        let opts = AddTorrentOptions {
            list_only: true,
            initial_peers: (!peers.is_empty()).then_some(peers),
            disabled_discovery: disabled,
            ..Default::default()
        };
        match self.session.add_torrent(AddTorrent::from_url(magnet), Some(opts)).await? {
            AddTorrentResponse::ListOnly(r) => {
                Ok(ResolvedMetadata { torrent_bytes: r.torrent_bytes.to_vec(), seen_peers: r.seen_peers })
            }
            _ => bail!("unexpected engine response"),
        }
    }

    /// Removes the torrent from the engine; with `delete_files` its files
    /// are deleted (or handed to `trash`).
    pub async fn remove(&self, h: &EngineHandle, delete_files: bool, trash: Option<Box<librqbit::storage::TrashFn>>) -> anyhow::Result<()> {
        self.session.delete_with_options(TorrentIdOrHash::Id(h.id()), DeleteOptions { delete_files, trash }).await
    }

    pub async fn pause(&self, h: &EngineHandle) {
        if !h.is_paused() {
            let _ = self.session.pause(h).await;
        }
    }

    pub async fn resume(&self, h: &EngineHandle) -> anyhow::Result<()> {
        if h.with_state(|s| matches!(s, ManagedTorrentState::Live(_))) {
            return Ok(());
        }
        self.session.unpause(h).await
    }

    /// Running = not paused and either live or checking.
    pub fn is_running(&self, h: &EngineHandle) -> bool {
        !h.is_paused()
            && h.with_state(|s| matches!(s, ManagedTorrentState::Live(_) | ManagedTorrentState::Initializing(_)))
    }

    pub fn status(&self, h: &EngineHandle) -> EngineStatus {
        let st = h.stats();
        let phase = match st.state {
            librqbit::TorrentStatsState::Initializing { .. } => EnginePhase::Initializing { checked_bytes: st.progress_bytes },
            librqbit::TorrentStatsState::Paused => EnginePhase::Paused,
            librqbit::TorrentStatsState::Live => EnginePhase::Live,
            librqbit::TorrentStatsState::Error => {
                let kind = match h.error_kind() {
                    Some(TorrentErrorKind::MissingFiles) => ErrorKind::MissingFiles,
                    Some(TorrentErrorKind::DiskFull) => ErrorKind::DiskFull,
                    _ => ErrorKind::Other,
                };
                EnginePhase::Error(st.error.clone().unwrap_or_default(), kind)
            }
        };
        let mut out = EngineStatus {
            phase,
            file_progress: st.file_progress,
            finished: st.finished,
            dl_rate: 0,
            ul_rate: 0,
            fetched: 0,
            uploaded: 0,
            peers_live: 0,
            seeders_live: 0,
            moving: h.move_progress().is_some(),
            wasted: h.wasted_bytes(),
            connections: h.connections(),
        };
        if let Some(live) = st.live {
            out.dl_rate = live.download_speed.as_bytes() as i64;
            out.ul_rate = live.upload_speed.as_bytes() as i64;
            out.fetched = live.snapshot.fetched_bytes;
            out.uploaded = live.snapshot.uploaded_bytes;
            out.peers_live = live.snapshot.peer_stats.live;
            out.seeders_live = live.snapshot.peer_stats.live_seeders;
        }
        out
    }

    pub fn piece_states(&self, h: &EngineHandle) -> Vec<u8> {
        h.piece_states()
    }

    pub fn piece_availability(&self, h: &EngineHandle) -> Vec<u32> {
        h.piece_availability()
    }

    pub fn distributed_copies(&self, h: &EngineHandle) -> f64 {
        h.distributed_copies()
    }

    /// SHA-1 piece hashes; for a pure v2 torrent (no SHA-1 hashes) the
    /// SHA-256 merkle hash of each piece instead (empty while its file's
    /// piece layer isn't known yet).
    pub fn piece_hashes(&self, h: &EngineHandle) -> Option<Vec<String>> {
        h.with_metadata(|m| match (m.info.v2_layout(), m.info.v2_piece_hashes()) {
            (Some(layout), Some(v2)) => (0..layout.total_pieces())
                .map(|p| v2.get(layout, p).map(|h| super::metainfo::hex(&h)).unwrap_or_default())
                .collect(),
            _ => m.info.info().pieces.as_ref().chunks(20).map(super::metainfo::hex).collect(),
        })
        .ok()
    }

    pub fn peers(&self, h: &EngineHandle) -> Vec<librqbit::PeerDetails> {
        h.peers_detailed()
    }

    pub fn add_peers(&self, h: &EngineHandle, peers: Vec<SocketAddr>) -> usize {
        h.add_peers(peers)
    }

    // ---- Web seeds ----------------------------------------------------------------

    pub fn web_seed_urls(&self, h: &EngineHandle) -> Vec<String> {
        h.web_seed_urls()
    }

    pub fn add_web_seeds(&self, h: &EngineHandle, urls: Vec<String>) -> usize {
        h.add_web_seeds(urls)
    }

    pub fn remove_web_seeds(&self, h: &EngineHandle, urls: &[String]) -> usize {
        h.remove_web_seeds(urls)
    }

    pub fn edit_web_seed(&self, h: &EngineHandle, old: &str, new: &str) -> Result<(), librqbit::WebSeedEditError> {
        h.edit_web_seed(old, new)
    }

    // ---- Trackers -------------------------------------------------------------

    pub fn trackers(&self, h: &EngineHandle) -> TrackerSummary {
        let (swarm_seeds, swarm_leechers) = h.swarm_counts();
        TrackerSummary { statuses: h.trackers(), swarm_seeds, swarm_leechers }
    }

    pub fn tracker_list(&self, h: &EngineHandle) -> Vec<(String, u32)> {
        h.tracker_list()
    }

    pub fn add_trackers(&self, h: &EngineHandle, entries: Vec<(String, u32)>) {
        h.add_trackers(entries);
    }

    pub fn remove_trackers(&self, h: &EngineHandle, urls: &[String]) {
        h.remove_trackers(urls);
    }

    pub fn edit_tracker(&self, h: &EngineHandle, old: &str, new: &str) -> Result<(), String> {
        h.edit_tracker(old, new).map_err(|e| format!("{e:?}"))
    }

    /// New URLs are trusted by SSRF mitigation only if in `trusted_urls`.
    pub fn replace_trackers(&self, h: &EngineHandle, entries: Vec<(String, u32)>, trusted_urls: &[String]) {
        h.replace_trackers_with_trust(entries, trusted_urls);
    }

    pub fn force_reannounce(&self, h: &EngineHandle, url: Option<&str>) {
        h.force_reannounce(url);
    }

    pub fn reannounce_all(&self) {
        self.session.reannounce_all();
    }

    // ---- Storage ----------------------------------------------------------------

    pub async fn move_storage(&self, h: &EngineHandle, to: PathBuf, overwrite: bool) -> anyhow::Result<()> {
        h.move_storage(to, if overwrite { MoveMode::AlwaysReplace } else { MoveMode::DontReplace }).await
    }

    pub fn output_folder(&self, h: &EngineHandle) -> PathBuf {
        h.output_folder()
    }

    /// Renames files by engine index to new relative paths.
    pub async fn rename_files(&self, h: &EngineHandle, renames: Vec<(usize, PathBuf)>) -> anyhow::Result<()> {
        h.rename_files(renames).await
    }

    pub fn actual_file_paths(&self, h: &EngineHandle) -> Vec<PathBuf> {
        h.actual_file_paths()
    }

    pub async fn force_recheck(&self, h: &EngineHandle) -> anyhow::Result<()> {
        h.force_recheck().await
    }

    pub fn set_file_priorities(&self, h: &EngineHandle, priorities: Vec<u8>) -> anyhow::Result<()> {
        h.set_file_priorities(priorities)
    }

    // ---- Session ------------------------------------------------------------------

    pub fn transfer_stats(&self) -> EngineTransferStats {
        let s = self.session.transfer_stats();
        EngineTransferStats {
            payload_downloaded: s.payload_downloaded,
            payload_uploaded: s.payload_uploaded,
            total_downloaded: s.protocol_downloaded + s.ip_overhead_downloaded,
            total_uploaded: s.protocol_uploaded + s.ip_overhead_uploaded,
            wasted: s.wasted_bytes,
            payload_dl_rate: s.payload_download_rate as i64,
            payload_ul_rate: s.payload_upload_rate as i64,
            peers: s.connected_peers,
            dht_nodes: s.dht_nodes as i64,
        }
    }

    /// Removes fast-resume files of torrents the core no longer knows.
    pub fn prune_fastresume(engine_dir: &Path, keep: &std::collections::HashSet<String>) {
        let dir = engine_dir.join("fastresume");
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or("");
            if stem.len() == 40 && stem.bytes().all(|b| b.is_ascii_hexdigit()) && !keep.contains(&stem.to_ascii_lowercase()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Tracker state as the model enum.
pub fn tracker_state(s: TrackerState) -> crate::model::TrackerStatus {
    use crate::model::TrackerStatus as M;
    match s {
        TrackerState::NotContacted => M::NotContacted,
        TrackerState::Working => M::Working,
        TrackerState::NotWorking => M::NotWorking,
        TrackerState::TrackerError => M::TrackerError,
        TrackerState::Unreachable => M::Unreachable,
    }
}

pub fn is_utp(k: ConnectionKind) -> bool {
    matches!(k, ConnectionKind::Utp)
}

pub fn is_i2p(k: ConnectionKind) -> bool {
    matches!(k, ConnectionKind::I2p)
}

pub fn has_source(p: &librqbit::PeerDetails, s: PeerSource) -> bool {
    p.sources.contains(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_peer_repeats_are_dropped() {
        let mut f = RepeatFilter::default();
        let t = std::time::Instant::now();
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        assert!(f.should_log(ip, t));
        assert!(!f.should_log(ip, t + Duration::from_millis(500)));
        assert!(f.should_log("1.2.3.5".parse().unwrap(), t));
        assert!(f.should_log(ip, t + Duration::from_secs(2)));
        for i in 0..10_000u32 {
            f.should_log(IpAddr::from(i.to_be_bytes()), t);
        }
        assert!(f.last.len() <= RepeatFilter::MAX_TRACKED);
    }

    #[test]
    fn live_settings_map_utp_mixed_mode() {
        let mut p = Preferences::default();
        assert_eq!(LiveSettings::from_prefs(&p).peer_policy.mixed_mode, MixedModeAlgorithm::PreferTcp);
        p.advanced.utp_tcp_mixed_mode = UtpMixedMode::PeerProportional;
        assert_eq!(LiveSettings::from_prefs(&p).peer_policy.mixed_mode, MixedModeAlgorithm::PeerProportional);
    }

    #[test]
    fn net_prefs_map_encryption_protocols_and_ranges() {
        let mut p = Preferences::default();
        p.bittorrent.encryption = EncryptionMode::Require;
        p.bittorrent.anonymous_mode = true;
        p.bittorrent.pex = false;
        p.connection.bittorrent_protocol = BtProtocol::Utp;
        p.advanced.outgoing_ports_min = 5000;
        p.advanced.outgoing_ports_max = 5100;
        p.advanced.peer_tos = 40;
        p.advanced.socket_send_buffer_size = 65536;
        p.advanced.connection_speed = 7;
        let n = NetPrefs::from_prefs(&p).net;
        assert_eq!(n.encryption, EncryptionPolicy::Forced);
        assert!(n.anonymous_mode && !n.pex);
        assert!(!n.enable_tcp && n.enable_utp);
        assert_eq!(n.outgoing_ports, Some((5000, 5100)));
        assert_eq!((n.peer_tos, n.send_buffer_size, n.recv_buffer_size, n.connection_speed), (40, 65536, 0, 7));
        // An inverted or half-set range is "disabled".
        p.advanced.outgoing_ports_max = 4000;
        assert_eq!(NetPrefs::from_prefs(&p).net.outgoing_ports, None);
    }

    #[test]
    fn net_prefs_proxy_follows_bittorrent_and_peer_switches() {
        let mut p = Preferences::default();
        p.connection.proxy_type = ProxyType::Socks4;
        p.connection.proxy_ip = " proxy.local ".into();
        p.connection.proxy_port = 1080;
        p.connection.proxy_auth_enabled = true;
        p.connection.proxy_username = "u".into();
        p.connection.proxy_password = "p".into();
        assert!(NetPrefs::from_prefs(&p).net.proxy.is_none(), "proxy_bittorrent is off");
        p.connection.proxy_bittorrent = true;
        let s = NetPrefs::from_prefs(&p).net.proxy.unwrap();
        assert_eq!(s.config.kind, ProxyKind::Socks4);
        assert_eq!((s.config.host.as_str(), s.config.port), ("proxy.local", 1080));
        assert_eq!(s.config.auth, Some(("u".into(), "p".into())));
        assert!(s.tracker_connections && !s.peer_connections);
        p.connection.proxy_peer_connections = true;
        assert!(NetPrefs::from_prefs(&p).net.proxy.unwrap().peer_connections);
        p.connection.proxy_ip.clear();
        assert!(NetPrefs::from_prefs(&p).net.proxy.is_none());
    }

    #[test]
    fn net_prefs_port_mappings_and_i2p() {
        let mut p = Preferences::default();
        p.connection.upnp = true;
        p.advanced.upnp_lease_duration = 3600;
        p.webui.web_ui_enabled = true;
        p.webui.web_ui_upnp = true;
        p.webui.web_ui_port = 8081;
        p.advanced.enable_embedded_tracker = true;
        p.advanced.embedded_tracker_port_forwarding = true;
        p.advanced.embedded_tracker_port = 9001;
        p.connection.i2p_enabled = true;
        p.connection.i2p_inbound_length = 99;
        p.connection.i2p_mixed_mode = true;
        let n = NetPrefs::from_prefs(&p);
        assert!(n.port_mapping.upnp && n.port_mapping.natpmp);
        assert_eq!(n.port_mapping.lease_secs, 3600);
        assert_eq!(n.extra_ports, vec![(MappingProtocol::Tcp, 8081), (MappingProtocol::Tcp, 9001)]);
        let i2p = n.i2p.unwrap();
        assert!(i2p.mixed_mode);
        assert_eq!(i2p.sam.inbound_length, 7);
        p.webui.web_ui_upnp = false;
        p.connection.i2p_enabled = false;
        let n = NetPrefs::from_prefs(&p);
        assert_eq!(n.extra_ports, vec![(MappingProtocol::Tcp, 9001)]);
        assert!(n.i2p.is_none());
    }

    #[test]
    fn only_dht_bootstrap_changes_restart_the_engine() {
        let base = EngineConfig { dht: true, bootstrap_nodes: vec!["a:1".into()], engine_dir: "/e".into(), client_name: "c".into() };
        assert!(!base.restart_required(&base));
        let other = EngineConfig { bootstrap_nodes: vec!["b:1".into()], ..base.clone() };
        assert!(other.restart_required(&base));
        // Enabling DHT on a session that started without it.
        let off = EngineConfig { dht: false, ..base.clone() };
        assert!(base.restart_required(&off));
        // Turning it off, or changing the list while it never ran, does not.
        assert!(!off.restart_required(&base));
        assert!(!EngineConfig { dht: false, bootstrap_nodes: vec!["b:1".into()], ..base.clone() }.restart_required(&off));
    }
}
