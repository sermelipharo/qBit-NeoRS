use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    net::{IpAddr, SocketAddr},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use crate::{
    ApiError, CreateTorrentOptions, FileInfos, ManagedTorrent, ManagedTorrentShared,
    api::TorrentIdOrHash,
    api_error::WithStatus,
    bitv_factory::{BitVFactory, NonPersistentBitVFactory},
    create_torrent,
    create_torrent_file::CreateTorrentResult,
    dht_utils::{ReadMetainfoResult, read_metainfo_from_peer_receiver},
    ip_ranges::IpRanges,
    limits::{Limits, LimitsConfig},
    listen::{Accept, ListenerOptions},
    merge_streams::merge_streams,
    peer_connection::PeerConnectionOptions,
    peer_policy::{PeerPolicyOptions, SessionPeerPolicy},
    peer_source::{DisabledDiscovery, PeerSource, SourcedPeerStream},
    read_buf::ReadBuf,
    session_persistence::{SessionPersistenceStore, json::JsonSessionPersistenceStore},
    session_stats::SessionStats,
    session_trackers::{SessionTrackers, SessionTrackersArgs, TrackerSettings},
    spawn_utils::BlockingSpawner,
    storage::{
        BoxStorageFactory, StorageFactoryExt, StorageLayout, StorageSettings, TorrentStorage,
        TrashFn, check_limiter::CheckingLimiter, filesystem::FilesystemStorageFactory,
        paths::sanitize_component, settings::StorageContext,
    },
    net::{
        NetSettings, PortMappingConfig,
        encryption::{Duplex, Req2Index, split_mse},
        external_ip::ExternalIpVoter,
        filter::PeerFilter,
        port_mapping::PortMapper,
        proxy_from_url,
        rate::ConnectRateLimiter,
    },
    stream_connect::{ConnectionKind, ConnectionOptions, StreamConnector, StreamConnectorArgs},
    torrent_state::{
        ManagedTorrentHandle, ManagedTorrentLocked, ManagedTorrentOptions, ManagedTorrentState,
        TorrentMetadata, TorrentStateLive, initializing::TorrentStateInitializing,
        live::priorities::file_priorities_from_only_files, runtime::TorrentRuntime,
        trackers::{TorrentTrackers, record_and_strip},
        webseeds::{TorrentWebSeeds, url_list_from_torrent, web_seeds_from_magnet},
    },
    type_aliases::{BoxAsyncReadVectored, BoxAsyncWrite, PeerStream},
};
use anyhow::{Context, bail};
use arc_swap::ArcSwapOption;
use bencode::bencode_serialize_to_writer;
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use clone_to_owned::CloneToOwned;
use dht::{Dht, DhtPersistenceConfig, DhtState, Id20};
use librqbit_core::hash_id::Id32;
use futures::{
    FutureExt, Stream, StreamExt,
    future::BoxFuture,
    stream::{BoxStream, FuturesUnordered},
};
use http::StatusCode;
use itertools::Itertools;
use librqbit_core::{
    crate_version,
    directories::get_configuration_directory,
    magnet::Magnet,
    peer_id::generate_azereus_style,
    spawn_utils::spawn_with_cancel,
    torrent_metainfo::{TorrentMetaV1Owned, ValidatedTorrentMetaV1Info},
};
use librqbit_lsd::LocalServiceDiscovery;
use librqbit_utp::BindDevice;
use parking_lot::RwLock;
use peer_binary_protocol::Handshake;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::{CancellationToken, DropGuard};
use tracing::{Instrument, debug, debug_span, error, info, trace, warn};
use tracker_comms::{
    HttpClientFactory, HttpClientOptions, IpFilterFn, TrackerList, TrackerOrigin, UdpTrackerClient,
};

mod networking;

pub const SUPPORTED_SCHEMES: [&str; 3] = ["http:", "https:", "magnet:"];

pub type TorrentId = usize;

/// Connection limit of a torrent when neither the torrent nor the session sets one.
const DEFAULT_PEER_LIMIT_PER_TORRENT: usize = 128;

struct ParsedTorrentFile {
    meta: TorrentMetaV1Owned,
    torrent_bytes: Bytes,
}

fn torrent_from_bytes(bytes: Bytes) -> anyhow::Result<ParsedTorrentFile> {
    trace!(
        "all fields in torrent: {:#?}",
        bencode::dyn_from_bytes::<ByteBuf>(&bytes)
    );
    let parsed = librqbit_core::torrent_metainfo::torrent_from_bytes(&bytes)?;
    Ok(ParsedTorrentFile {
        meta: parsed.clone_to_owned(Some(&bytes)),
        torrent_bytes: bytes,
    })
}

#[derive(Default)]
pub struct SessionDatabase {
    torrents: HashMap<TorrentId, ManagedTorrentHandle>,
}

impl SessionDatabase {
    fn add_torrent(&mut self, torrent: ManagedTorrentHandle, id: TorrentId) {
        self.torrents.insert(id, torrent);
    }
}

pub struct Session {
    // Core state and services
    pub(crate) db: RwLock<SessionDatabase>,
    next_id: AtomicUsize,
    pub(crate) bitv_factory: Arc<dyn BitVFactory>,
    spawner: BlockingSpawner,

    // Network
    peer_id: Id20,
    /// Peer id used in anonymous mode (generic prefix, random).
    anon_peer_id: Id20,
    /// Listeners (rebindable at runtime, see `networking.rs`).
    listen: RwLock<networking::ListenState>,
    listen_lock: tokio::sync::Mutex<()>,
    /// Set once any incoming peer connection passed its handshake.
    incoming_seen: std::sync::atomic::AtomicBool,
    dht: ArcSwapOption<DhtState>,
    dht_ctl: parking_lot::Mutex<networking::ServiceCtl<networking::DhtRestartConfig>>,
    /// Bumped when DHT/LSD are switched or the announce port changes; the
    /// torrents' DHT/LSD peer streams restart on it.
    discovery_gen: tokio::sync::watch::Sender<u64>,
    bind_device: RwLock<Option<BindDevice>>,
    /// Proxy and interface used by HTTP clients built later (trackers).
    http_env: Arc<networking::HttpEnv>,
    pub(crate) connector: Arc<StreamConnector>,
    reqwest_client: reqwest::Client,
    /// Memory for piece layers being fetched from peers (all torrents).
    pub(crate) fetch_budget: Arc<crate::peer_info_reader::budget::FetchBudget>,
    /// HTTP client for web seeds (replaced when proxy settings change).
    webseed_http: arc_swap::ArcSwap<WebSeedClients>,
    /// Concurrent web seed fetches of the whole session.
    pub(crate) webseed_fetches: Arc<tokio::sync::Semaphore>,
    pub(crate) tracker_ctx: Arc<SessionTrackers>,
    pub(crate) peer_filter: PeerFilter,
    pub(crate) req2_index: Req2Index,
    pub(crate) connect_limiter: Arc<ConnectRateLimiter>,
    port_mapper: PortMapper,
    /// Our public address, voted on by the gateway, trackers and peers.
    pub(crate) external_ip: Arc<ExternalIpVoter>,
    pub(crate) i2p: crate::i2p::I2pSessionCtl,

    // Lifecycle management
    cancellation_token: CancellationToken,
    _cancellation_token_drop_guard: DropGuard,

    // Runtime settings
    output_folder: PathBuf,
    peer_opts: PeerConnectionOptions,
    default_storage_factory: Option<BoxStorageFactory>,
    persistence: Option<Arc<dyn SessionPersistenceStore>>,
    trackers: HashSet<url::Url>,

    lsd: ArcSwapOption<LocalServiceDiscovery>,
    lsd_token: parking_lot::Mutex<Option<CancellationToken>>,

    // Limits and throttling
    /// Limits concurrent checks ("maximum active checking torrents").
    pub(crate) checking_limiter: Arc<CheckingLimiter>,
    /// Storage settings and the file handle pool.
    pub(crate) storage: Arc<StorageContext>,
    pub ratelimits: Limits,
    // Upload slots, choking, global connection limit, rate limit exemptions.
    pub(crate) peer_policy: SessionPeerPolicy,

    pub blocklist: IpRanges,
    pub allowlist: Option<IpRanges>,

    // Monitoring / tracing / logging
    pub(crate) stats: Arc<SessionStats>,
    root_span: Option<tracing::Span>,

    // Feature flags
    #[cfg(feature = "disable-upload")]
    _disable_upload: bool,
    pub ipv4_only: bool,
    pub peer_limit: Option<usize>,
    client_name_and_version: String,
}

/// A 40 character hex string is a v1 info hash. Checking the length alone
/// also matches URLs that happen to be 40 characters long, such as
/// https://nyaa.si/download/1234567.torrent.
fn is_bare_info_hash(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn torrent_from_url(
    reqwest_client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<ParsedTorrentFile> {
    let response = reqwest_client
        .get(url)
        .send()
        .await
        .context("error downloading torrent metadata")?;
    if !response.status().is_success() {
        bail!("GET {} returned {}", url, response.status())
    }
    let b = response
        .bytes()
        .await
        .with_context(|| format!("error reading response body from {url}"))?;
    torrent_from_bytes(b).context("error decoding torrent")
}

fn compute_only_files_regex<ByteBuf: AsRef<[u8]>>(
    torrent: &ValidatedTorrentMetaV1Info<ByteBuf>,
    filename_re: &str,
) -> anyhow::Result<Vec<usize>> {
    let filename_re = regex::Regex::new(filename_re).context("filename regex is incorrect")?;
    let mut only_files = Vec::new();
    for (idx, fd) in torrent.iter_file_details().enumerate() {
        let full_path = fd.filename.to_pathbuf();
        if filename_re.is_match(full_path.to_str().unwrap()) {
            only_files.push(idx);
        }
    }
    if only_files.is_empty() {
        bail!("none of the filenames match the given regex")
    }
    Ok(only_files)
}

fn compute_only_files(
    info: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
    only_files: Option<Vec<usize>>,
    only_files_regex: Option<String>,
    list_only: bool,
) -> anyhow::Result<Option<Vec<usize>>> {
    match (only_files, only_files_regex) {
        (Some(_), Some(_)) => {
            bail!("only_files and only_files_regex are mutually exclusive");
        }
        (Some(only_files), None) => {
            let total_files = info.iter_file_lengths().count();
            for id in only_files.iter().copied() {
                if id >= total_files {
                    bail!("file id {} is out of range", id);
                }
            }
            Ok(Some(only_files))
        }
        (None, Some(filename_re)) => {
            let only_files = compute_only_files_regex(info, &filename_re)?;
            for (idx, fd) in info.iter_file_details().enumerate() {
                if !only_files.contains(&idx) {
                    continue;
                }
                if !list_only {
                    info!(filename=?fd.filename, "will download");
                }
            }
            Ok(Some(only_files))
        }
        (None, None) => Ok(None),
    }
}

/// (v1 hash, truncated v2 hash) of a hybrid torrent; `None` for v1-only
/// and pure v2 torrents.
fn hybrid_hashes(m: &TorrentMetadata) -> Option<(Id20, Id20)> {
    use sha1w::ISha1;
    if m.info.info().meta_version != Some(2) || m.info.is_pure_v2() {
        return None;
    }
    let mut v1 = sha1w::Sha1::new();
    v1.update(&m.info_bytes);
    let v2 = Id32::new(librqbit_core::v2::sha256(&m.info_bytes));
    Some((Id20::new(v1.finish()), v2.truncate_for_dht()))
}

fn merge_two_optional_streams<T>(
    s1: Option<impl Stream<Item = T> + Unpin + Send + 'static>,
    s2: Option<impl Stream<Item = T> + Unpin + Send + 'static>,
) -> Option<BoxStream<'static, T>> {
    match (s1, s2) {
        (Some(s1), None) => Some(Box::pin(s1)),
        (None, Some(s2)) => Some(Box::pin(s2)),
        (Some(s1), Some(s2)) => Some(Box::pin(merge_streams(s1, s2))),
        (None, None) => None,
    }
}

/// Options for adding new torrents to the session.
//
// Serialize/deserialize is for Tauri.
#[derive(Default, Serialize, Deserialize)]
pub struct AddTorrentOptions {
    /// Start in paused state.
    #[serde(default)]
    pub paused: bool,
    /// A regex to only download files matching it.
    pub only_files_regex: Option<String>,
    /// An explicit list of file IDs to download.
    /// To see the file indices, run with "list_only".
    pub only_files: Option<Vec<usize>>,
    /// Allow writing on top of existing files, including when resuming a torrent.
    /// You probably want to set it, however for safety it's not default.
    ///
    /// Even when all the torrent pieces have been written, `overwrite` needs to
    /// be enabled in order to resume/seed the torrent.
    #[serde(default)]
    pub overwrite: bool,
    /// Only list the files in the torrent without starting it.
    #[serde(default)]
    pub list_only: bool,
    /// The output folder for the torrent. If not set, the session's default one will be used.
    pub output_folder: Option<String>,
    /// Sub-folder within session's default output folder. Will error if "output_folder" if also set.
    /// By default, multi-torrent files are downloaded to a sub-folder.
    pub sub_folder: Option<String>,
    /// Peer connection options, timeouts etc. If not set, session's defaults will be used.
    pub peer_opts: Option<PeerConnectionOptions>,

    /// Force a refresh interval for polling trackers.
    pub force_tracker_interval: Option<Duration>,

    #[serde(default)]
    pub disable_trackers: bool,

    /// DHT/PeX/LSD switched off for this torrent (change it later with
    /// `ManagedTorrent::set_disabled_discovery`).
    #[serde(default)]
    pub disabled_discovery: DisabledDiscovery,

    #[serde(default)]
    pub ratelimits: LimitsConfig,

    /// Initial peers to start of with.
    pub initial_peers: Option<Vec<SocketAddr>>,

    /// Max concurrent connected peers.
    pub peer_limit: Option<usize>,

    /// This is used to restore the session from serialized state.
    pub preferred_id: Option<usize>,

    #[serde(skip)]
    pub storage_factory: Option<BoxStorageFactory>,

    /// Receives the torrent's tracker manager as soon as it exists, so a
    /// `list_only` add (fetching a magnet's metadata) can show its trackers'
    /// state while it runs.
    #[serde(skip)]
    pub tracker_manager_out: Option<Arc<std::sync::OnceLock<Arc<TorrentTrackers>>>>,

    /// Extra trackers, appended in one new tier after the torrent's own.
    pub trackers: Option<Vec<String>>,

    /// Extra trackers with explicit tiers, appended after the torrent's own.
    #[serde(default)]
    pub tracker_entries: Option<Vec<(String, u32)>>,

    /// Use exactly this `(url, tier)` list instead of the trackers from the
    /// torrent, `trackers`, `tracker_entries` and the session's extra trackers
    /// (e.g. to restore a persisted, edited list).
    #[serde(default)]
    pub replace_trackers: Option<Vec<(String, u32)>>,

    /// Whether the torrent comes from a local source, which makes its
    /// trackers trusted by SSRF mitigation. Default: true for torrent file
    /// bytes, false for URLs and magnet links.
    #[serde(default)]
    pub trackers_local_source: Option<bool>,
    /// Tracker and web seed URLs that are trusted even when the torrent is
    /// not from a local source (e.g. added by the user). Only consulted for
    /// `replace_trackers` and `replace_web_seeds`.
    #[serde(default)]
    pub trusted_urls: Option<Vec<String>>,
    /// Don't hash existing data on the first check: assume all pieces are present
    /// (like libtorrent's seed mode). Each piece is verified when it's first read;
    /// a piece that fails is marked missing and downloaded again.
    #[serde(default)]
    pub skip_hash_check: bool,

    /// File path overrides by file index (relative to the output folder), e.g. to
    /// restore renames made earlier with `ManagedTorrent::rename_file`.
    #[serde(default)]
    pub renamed_files: Option<std::collections::BTreeMap<usize, PathBuf>>,

    /// Whether the output folder belongs to this torrent, so that it's removed when
    /// left empty after deleting or moving the torrent's files. Defaults to true when
    /// the session derives the folder (default sub-folder or `sub_folder`), false
    /// for an explicit `output_folder`.
    #[serde(default)]
    pub owns_output_folder: Option<bool>,

    /// Extra web seeds (BEP 19), added after the torrent's `url-list` (trusted:
    /// the SSRF rule doesn't apply to them).
    #[serde(default)]
    pub web_seeds: Option<Vec<String>>,

    /// Use exactly these web seeds instead of the torrent's `url-list`, magnet
    /// `ws` parameters and `web_seeds` (e.g. to restore a persisted list).
    #[serde(default)]
    pub replace_web_seeds: Option<Vec<String>>,
}

/// Options for [`Session::delete_with_options`].
#[derive(Default)]
pub struct DeleteOptions {
    /// Also delete the torrent's files.
    pub delete_files: bool,
    /// Move files to the trash with this callback instead of deleting them.
    pub trash: Option<Box<TrashFn>>,
}

pub struct ListOnlyResponse {
    pub info_hash: Id20,
    pub info: ValidatedTorrentMetaV1Info<ByteBufOwned>,
    pub only_files: Option<Vec<usize>>,
    pub output_folder: PathBuf,
    pub seen_peers: Vec<SocketAddr>,
    pub torrent_bytes: Bytes,
}

#[allow(clippy::large_enum_variant)]
pub enum AddTorrentResponse {
    AlreadyManaged(TorrentId, ManagedTorrentHandle),
    ListOnly(ListOnlyResponse),
    Added(TorrentId, ManagedTorrentHandle),
}

impl AddTorrentResponse {
    pub fn into_handle(self) -> Option<ManagedTorrentHandle> {
        match self {
            Self::AlreadyManaged(_, handle) => Some(handle),
            Self::ListOnly(_) => None,
            Self::Added(_, handle) => Some(handle),
        }
    }
}

pub fn read_local_file_including_stdin(filename: &str) -> anyhow::Result<Vec<u8>> {
    let mut buf = Vec::new();
    if filename == "-" {
        std::io::stdin()
            .read_to_end(&mut buf)
            .context("error reading stdin")?;
    } else {
        std::fs::File::open(filename)
            .context("error opening")?
            .read_to_end(&mut buf)
            .context("error reading")?;
    }
    Ok(buf)
}

pub enum AddTorrent<'a> {
    Url(Cow<'a, str>),
    TorrentFileBytes(Bytes),
}

impl<'a> AddTorrent<'a> {
    // Don't call this from HTTP API.
    #[inline(never)]
    pub fn from_cli_argument(path: &'a str) -> anyhow::Result<Self> {
        if SUPPORTED_SCHEMES.iter().any(|s| path.starts_with(s)) {
            return Ok(Self::Url(Cow::Borrowed(path)));
        }
        if path.len() == 40 && !Path::new(path).exists() && Magnet::parse(path).is_ok() {
            return Ok(Self::Url(Cow::Borrowed(path)));
        }
        Self::from_local_filename(path)
    }

    pub fn from_url(url: impl Into<Cow<'a, str>>) -> Self {
        Self::Url(url.into())
    }

    pub fn from_bytes(bytes: impl Into<Bytes>) -> Self {
        Self::TorrentFileBytes(bytes.into())
    }

    // Don't call this from HTTP API.
    #[inline(never)]
    pub fn from_local_filename(filename: &str) -> anyhow::Result<Self> {
        let file = read_local_file_including_stdin(filename)
            .with_context(|| format!("error reading local file {filename:?}"))?;
        Ok(Self::TorrentFileBytes(file.into()))
    }

    pub fn into_bytes(self) -> Bytes {
        match self {
            Self::Url(s) => s.into_owned().into_bytes().into(),
            Self::TorrentFileBytes(b) => b,
        }
    }
}

pub enum SessionPersistenceConfig {
    /// The filename for persistence. By default uses an OS-specific folder.
    Json { folder: Option<PathBuf> },
    /// Keeps only the fast-resume bitfields of the torrents in the folder; no
    /// session database and no torrent file copies (the embedder re-adds its
    /// torrents itself).
    FastResumeOnly { folder: PathBuf },
    #[cfg(feature = "postgres")]
    Postgres { connection_string: String },
}

impl SessionPersistenceConfig {
    pub fn default_json_persistence_folder() -> anyhow::Result<PathBuf> {
        let dir = get_configuration_directory("session")?;
        Ok(dir.data_dir().to_owned())
    }
}

/// Configuration for the DHT subsystem.
/// Set to `None` in `SessionOptions::dht` to disable DHT entirely.
pub struct DhtSessionConfig {
    /// Bootstrap nodes (host:port or ip:port). Uses built-in defaults if None.
    pub bootstrap_addrs: Option<Vec<String>>,
    /// The DHT listen port. Priority: this explicit port -> persisted port
    /// (when persistence is enabled) -> random. The bind IP is derived from
    /// `SessionOptions::ipv4_only` (`0.0.0.0` if true, `[::]` otherwise).
    /// Use `SessionOptions::bind_device_name` to scope the bind to a specific
    /// network interface.
    pub port: Option<u16>,
    /// Persistence behavior. If None, persistence is disabled.
    pub persistence: Option<DhtPersistenceConfig>,
}

impl Default for DhtSessionConfig {
    fn default() -> Self {
        Self {
            bootstrap_addrs: None,
            port: None,
            persistence: Some(DhtPersistenceConfig::default()),
        }
    }
}

pub struct SessionOptions {
    /// DHT configuration. Set to None to disable DHT entirely.
    /// Defaults to DHT enabled with persistence.
    pub dht: Option<DhtSessionConfig>,

    /// What network device to bind to for DHT, BT-UDP, BT-TCP, trackers and LSD.
    /// On OSX will use IP(V6)_BOUND_IF, on Linux will use SO_BINDTODEVICE.
    pub bind_device_name: Option<String>,

    /// Disable tracker communication
    pub disable_trackers: bool,

    /// Enable fastresume, to restore state quickly after restart.
    pub fastresume: bool,

    /// Turn on to dump session contents into a file periodically, so that on next start
    /// all remembered torrents will continue where they left off.
    pub persistence: Option<SessionPersistenceConfig>,

    /// The peer ID to use. If not specified, a random one will be generated.
    pub peer_id: Option<Id20>,

    /// Options for listening on TCP and/or uTP for incoming connections.
    pub listen: Option<ListenerOptions>,
    /// Options for connecting to peers (for outgiong connections).
    pub connect: Option<ConnectionOptions>,

    pub default_storage_factory: Option<BoxStorageFactory>,

    pub cancellation_token: Option<CancellationToken>,

    /// How many torrents can check their data at the same time (default 3).
    /// Adjustable at runtime with `Session::set_max_active_checking_torrents`.
    pub concurrent_init_limit: Option<usize>,

    /// Storage settings (incomplete suffix, unwanted folder, preallocation, file
    /// handle pool, OS cache). Adjustable at runtime with `Session::set_storage_settings`.
    pub storage: StorageSettings,

    /// How many blocking threads does the tokio runtime have.
    /// Will limit blocking work to that number to avoid starving the runtime.
    pub runtime_worker_threads: Option<usize>,

    /// the root span to use. If not set will be None.
    pub root_span: Option<tracing::Span>,

    pub ratelimits: LimitsConfig,

    pub blocklist_url: Option<String>,
    pub allowlist_url: Option<String>,

    // The list of tracker URLs to always use for each torrent.
    pub trackers: HashSet<url::Url>,

    /// Default peer limit per torrent.
    pub peer_limit: Option<usize>,

    #[cfg(feature = "disable-upload")]
    pub disable_upload: bool,

    /// Disable LSD multicast
    pub disable_local_service_discovery: bool,

    /// Force IPv4 only.
    pub ipv4_only: bool,

    /// Override the client name and version used in User-Agent headers and
    /// peer extended handshakes. Defaults to "rqbit X.Y.Z".
    pub client_name_and_version: Option<String>,

    /// Upload slots, choking algorithms, the global connection limit and rate
    /// limit exemptions. Defaults to unlimited slots and connections.
    pub peer_policy: PeerPolicyOptions,
    /// Tracker settings (runtime-changeable with `Session::set_tracker_settings`).
    pub tracker_settings: TrackerSettings,
    /// Encryption, proxy, anonymous mode, socket tuning, connection speed and
    /// PeX (runtime-changeable with `Session::set_net_settings`). A proxy here
    /// takes precedence over `ConnectionOptions::proxy_url`.
    pub net: NetSettings,
    /// UPnP / NAT-PMP port mapping; `None` = derived from
    /// `ListenerOptions::enable_upnp_port_forwarding` (UPnP and NAT-PMP).
    pub port_mapping: Option<PortMappingConfig>,
    /// I2P through a SAM bridge; `None` = off (see `Session::set_i2p`).
    pub i2p: Option<crate::i2p::I2pSettings>,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            dht: Some(DhtSessionConfig::default()),
            bind_device_name: None,
            disable_trackers: false,
            fastresume: false,
            persistence: None,
            peer_id: None,
            listen: None,
            connect: None,
            default_storage_factory: None,
            cancellation_token: None,
            concurrent_init_limit: None,
            runtime_worker_threads: None,
            root_span: None,
            ratelimits: LimitsConfig::default(),
            blocklist_url: None,
            allowlist_url: None,
            trackers: HashSet::new(),
            peer_limit: None,
            #[cfg(feature = "disable-upload")]
            disable_upload: false,
            disable_local_service_discovery: false,
            ipv4_only: false,
            client_name_and_version: None,
            peer_policy: PeerPolicyOptions::default(),
            tracker_settings: TrackerSettings::default(),
            storage: StorageSettings::default(),
            net: NetSettings::default(),
            port_mapping: None,
            i2p: None,
        }
    }
}

fn torrent_file_from_info_bytes(
    info_bytes: &[u8],
    piece_layers: Option<&[u8]>,
    trackers: &[(String, u32)],
) -> anyhow::Result<Bytes> {
    #[derive(Serialize)]
    struct Tmp<'a> {
        announce: &'a str,
        #[serde(rename = "announce-list")]
        announce_list: Vec<Vec<&'a str>>,
        info: bencode::raw_value::RawValue<&'a [u8]>,
        #[serde(rename = "piece layers", skip_serializing_if = "Option::is_none")]
        piece_layers: Option<bencode::raw_value::RawValue<&'a [u8]>>,
    }

    let mut tiers: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for (url, tier) in trackers {
        tiers.entry(*tier).or_default().push(url.as_str());
    }
    let mut w = Vec::new();
    let v = Tmp {
        info: bencode::raw_value::RawValue(info_bytes),
        piece_layers: piece_layers.map(bencode::raw_value::RawValue),
        announce: trackers.first().map(|s| s.0.as_str()).unwrap_or(""),
        announce_list: tiers.into_values().collect(),
    };
    bencode_serialize_to_writer(&v, &mut w)?;
    Ok(w.into())
}

/// The initial tracker list of a new torrent: the torrent's own trackers,
/// then `opts.trackers` (one new tier), `opts.tracker_entries` and, for public
/// torrents, the session's extra trackers (one new tier). `opts.replace_trackers`
/// overrides all of them.
fn initial_tracker_list(
    source: &[(String, u32)],
    opts: &mut AddTorrentOptions,
    private: bool,
    local_source: bool,
    session_trackers: &HashSet<url::Url>,
) -> TrackerList {
    let mut list = TrackerList::new();
    if let Some(replace) = opts.replace_trackers.take() {
        let trusted_urls = opts.trusted_urls.as_deref().unwrap_or_default();
        for (url, tier) in replace {
            let trusted = local_source || trusted_urls.iter().any(|u| u.trim() == url.trim());
            list.add(&url, tier, TrackerOrigin::Torrent, trusted);
        }
        return list;
    }
    for (url, tier) in source {
        list.add(url, *tier, TrackerOrigin::Torrent, local_source);
    }
    if let Some(extra) = opts.trackers.take() {
        let tier = list.max_tier().map_or(0, |t| t.saturating_add(1));
        for url in extra {
            list.add(&url, tier, TrackerOrigin::Torrent, local_source);
        }
    }
    if let Some(entries) = opts.tracker_entries.take() {
        for (url, tier) in entries {
            list.add(&url, tier, TrackerOrigin::Torrent, local_source);
        }
    }
    if !private && !session_trackers.is_empty() {
        let tier = list.max_tier().map_or(0, |t| t.saturating_add(1));
        let mut extra: Vec<&url::Url> = session_trackers.iter().collect();
        extra.sort();
        for url in extra {
            list.add(url.as_str(), tier, TrackerOrigin::Session, true);
        }
    }
    list
}

/// Concurrent web seed piece fetches of the session.
const MAX_WEBSEED_FETCHES: usize = 16;

/// Incoming handshakes (MSE, BitTorrent, torrent lookup) pending at once from
/// one address (IPv6: one /64).
pub(crate) const MAX_PENDING_HANDSHAKES_PER_IP: usize = 4;
/// Overall deadline of an incoming handshake (each read also has its own
/// timeout).
const INCOMING_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);

fn pending_handshake_key(ip: IpAddr) -> IpAddr {
    match crate::net::filter::canonical_ip(ip) {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

pub(crate) struct CheckedIncomingConnection {
    pub kind: ConnectionKind,
    pub addr: SocketAddr,
    pub reader: BoxAsyncReadVectored,
    pub writer: BoxAsyncWrite,
    pub read_buf: ReadBuf,
    pub handshake: Handshake,
    /// MSE/PE with RC4.
    pub encrypted: bool,
}

struct InternalAddResult {
    info_hash: Id20,
    /// A v2-only (`btmh`) magnet: `info_hash` is this hash truncated.
    info_hash_v2: Option<Id32>,
    metadata: Option<TorrentMetadata>,
    /// `(url, tier)` from the torrent's announce list or magnet `tr` params.
    trackers: Vec<(String, u32)>,
    /// Torrent bytes were handed to us (not fetched from a URL or a magnet).
    local_source: bool,
    name: Option<String>,
    /// Web seeds from a magnet link's `ws` parameters.
    web_seeds: Vec<String>,
}

/// Trackers of a torrent file: the announce-list tiers (BEP 12) or, without
/// one, the single announce URL.
fn metainfo_trackers(meta: &TorrentMetaV1Owned) -> Vec<(String, u32)> {
    let as_str = |b: &ByteBufOwned| match std::str::from_utf8(b.as_ref()) {
        Ok(url) => Some(url.trim().to_owned()),
        Err(_) => {
            warn!("cannot parse tracker url as utf-8, ignoring");
            None
        }
    };
    if meta.announce_list.iter().flatten().next().is_some() {
        meta.announce_list
            .iter()
            .enumerate()
            .flat_map(|(tier, urls)| {
                let tier = u32::try_from(tier).unwrap_or(u32::MAX);
                urls.iter().filter_map(as_str).map(move |u| (u, tier))
            })
            .collect()
    } else {
        meta.announce
            .iter()
            .filter_map(as_str)
            .map(|u| (u, 0))
            .collect()
    }
}

impl Session {
    /// Create a new session with default options.
    /// The passed in folder will be used as a default unless overridden per torrent.
    /// It will run a DHT server/client, a TCP listener and .
    #[inline(never)]
    pub fn new(default_output_folder: PathBuf) -> BoxFuture<'static, anyhow::Result<Arc<Self>>> {
        Self::new_with_opts(default_output_folder, SessionOptions::default())
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }

    /// Create a new session with options.
    #[inline(never)]
    pub fn new_with_opts(
        default_output_folder: PathBuf,
        mut opts: SessionOptions,
    ) -> BoxFuture<'static, anyhow::Result<Arc<Self>>> {
        async move {
            let peer_id = opts
                .peer_id
                .unwrap_or_else(|| generate_azereus_style(*b"rQ", crate_version!()));
            let token = opts.cancellation_token.take().unwrap_or_default();
            opts.storage
                .validate()
                .context("invalid storage settings")?;

            #[cfg(feature = "disable-upload")]
            if opts.disable_upload {
                warn!("uploading disabled");
            }

            let bind_device = match opts.bind_device_name.as_ref() {
                Some(name) => Some(
                    BindDevice::new_from_name(name)
                        .with_context(|| format!("error creating bind device {name}"))?,
                ),
                None => None,
            };

            let listen_opts = opts.listen.take();
            let dht_restart = opts
                .dht
                .take()
                .map(networking::DhtRestartConfig::from_session_config);
            let dht = match &dht_restart {
                Some(cfg) => {
                    let dht_token = token.child_token();
                    let dht = networking::start_dht(
                        cfg,
                        opts.ipv4_only,
                        bind_device.as_ref(),
                        dht_token.clone(),
                    )
                    .await?;
                    Some((dht, dht_token))
                }
                None => None,
            };
            let peer_opts = opts
                .connect
                .as_ref()
                .and_then(|p| p.peer_opts)
                .unwrap_or_default();

            async fn persistence_factory(
                opts: &SessionOptions,
                spawner: BlockingSpawner,
            ) -> anyhow::Result<(
                Option<Arc<dyn SessionPersistenceStore>>,
                Arc<dyn BitVFactory>,
            )> {
                macro_rules! make_result {
                    ($store:expr) => {
                        if opts.fastresume {
                            Ok((Some($store.clone()), $store))
                        } else {
                            Ok((Some($store), Arc::new(NonPersistentBitVFactory {})))
                        }
                    };
                }

                match &opts.persistence {
                    Some(SessionPersistenceConfig::Json { folder }) => {
                        let folder = match folder.as_ref() {
                            Some(f) => f.clone(),
                            None => SessionPersistenceConfig::default_json_persistence_folder()?,
                        };

                        let s = Arc::new(
                            JsonSessionPersistenceStore::new(folder, spawner, false)
                                .await
                                .context("error initializing JsonSessionPersistenceStore")?,
                        );

                        make_result!(s)
                    }
                    Some(SessionPersistenceConfig::FastResumeOnly { folder }) => {
                        let s = Arc::new(
                            JsonSessionPersistenceStore::new(folder.clone(), spawner, true)
                                .await
                                .context("error initializing JsonSessionPersistenceStore")?,
                        );

                        make_result!(s)
                    }
                    #[cfg(feature = "postgres")]
                    Some(SessionPersistenceConfig::Postgres { connection_string }) => {
                        use crate::session_persistence::postgres::PostgresSessionStorage;
                        let p = Arc::new(PostgresSessionStorage::new(connection_string).await?);
                        make_result!(p)
                    }
                    None => Ok((None, Arc::new(NonPersistentBitVFactory {}))),
                }
            }

            const DEFAULT_BLOCKING_THREADS_IF_NOT_SET: usize = 8;
            let spawner = BlockingSpawner::new(
                opts.runtime_worker_threads
                    .unwrap_or(DEFAULT_BLOCKING_THREADS_IF_NOT_SET),
            );

            let (persistence, bitv_factory) = persistence_factory(&opts, spawner.clone())
                .await
                .context("error initializing session persistence store")?;

            let mut net = opts.net.clone();
            if net.proxy.is_none()
                && let Some(pu) = opts.connect.as_ref().and_then(|s| s.proxy_url.as_ref())
            {
                net.proxy = Some(
                    proxy_from_url(pu).with_context(|| format!("error parsing proxy url {pu}"))?,
                );
            }
            if !opts.connect.as_ref().map(|c| c.enable_tcp).unwrap_or(true) {
                net.enable_tcp = false;
            }
            net.validate().context("invalid network settings")?;

            let client_name_and_version = opts
                .client_name_and_version
                .unwrap_or_else(|| crate::client_name_and_version().to_owned());

            let http_env = Arc::new(networking::HttpEnv::new(
                net.tracker_proxy().cloned(),
                opts.bind_device_name.clone(),
            ));

            let reqwest_client = http_env
                .builder()?
                .user_agent(&client_name_and_version)
                .build()
                .context("error building HTTP(S) client")?;

            let webseed_http = WebSeedClients::build(
                net.peer_proxy(),
                opts.bind_device_name.as_deref(),
                (!net.anonymous_mode).then_some(client_name_and_version.as_str()),
            )?;

            let stream_connector = Arc::new(
                StreamConnector::new(StreamConnectorArgs {
                    settings: net.clone(),
                    utp_socket: None,
                    bind_device: bind_device.clone(),
                    ipv4_only: opts.ipv4_only,
                })
                .context("error creating stream connector")?,
            );

            let blocklist = if let Some(blocklist_url) = opts.blocklist_url {
                info!(url = blocklist_url, "loading p2p blocklist");
                let bl = IpRanges::load_from_url(&reqwest_client, &blocklist_url)
                    .await
                    .with_context(|| format!("error reading blocklist from {blocklist_url}"))?;
                info!(len = bl.len(), "loaded blocklist");
                bl
            } else {
                IpRanges::default()
            };

            let allowlist = if let Some(allowlist_url) = opts.allowlist_url {
                info!(url = allowlist_url, "loading p2p allowlist");
                let al = IpRanges::load_from_url(&reqwest_client, &allowlist_url)
                    .await
                    .with_context(|| format!("error reading allowlist from {allowlist_url}"))?;
                info!(len = al.len(), "loaded allowlist");
                Some(al)
            } else {
                None
            };

            let udp_tracker_client = UdpTrackerClient::new(token.clone(), bind_device.as_ref())
                .await
                .context("error creating UDP tracker client")?;

            let tracker_http_factory: HttpClientFactory = {
                let env = http_env.clone();
                Arc::new(
                    move |b: reqwest::ClientBuilder, o: &HttpClientOptions| {
                        #[allow(unused_mut)]
                        let mut b = env.apply(b)?;
                        #[cfg(any(feature = "default-tls", feature = "rust-tls"))]
                        {
                            b = b.tls_danger_accept_invalid_certs(!o.validate_https_certificates);
                        }
                        #[cfg(not(any(feature = "default-tls", feature = "rust-tls")))]
                        let _ = o;
                        Ok(b.build()?)
                    },
                )
            };
            let anon_peer_id = generate_azereus_style(*b"LT", (2, 0, 0, 0));
            let mut tracker_settings = opts.tracker_settings.clone();
            if net.anonymous_mode {
                tracker_settings.anonymous_mode = true;
            }
            let external_ip = Arc::new(ExternalIpVoter::default());
            let tracker_ctx = Arc::new(
                SessionTrackers::new(SessionTrackersArgs {
                    settings: tracker_settings,
                    udp: udp_tracker_client,
                    http_factory: tracker_http_factory,
                    proxied: net.tracker_proxy().is_some(),
                    peer_id: if net.anonymous_mode {
                        anon_peer_id
                    } else {
                        peer_id
                    },
                    listen_port: None,
                    ipv4_only: opts.ipv4_only,
                    client_name: client_name_and_version.clone(),
                    disabled: opts.disable_trackers,
                    cancel: token.clone(),
                    external_ip: external_ip.clone(),
                })
                .context("error creating tracker client")?,
            );

            tracker_ctx
                .client
                .set_udp_disabled(net.tracker_proxy().is_some());

            let lsd = if opts.disable_local_service_discovery {
                None
            } else {
                let lsd_token = token.child_token();
                networking::start_lsd(bind_device.as_ref(), lsd_token.clone())
                    .await
                    .map(|l| (l, lsd_token))
            };

            let port_mapping = opts.port_mapping.clone().unwrap_or_else(|| {
                let on = listen_opts
                    .as_ref()
                    .is_some_and(|l| l.enable_upnp_port_forwarding);
                PortMappingConfig {
                    upnp: on,
                    natpmp: on,
                    ..Default::default()
                }
            });
            let (dht, dht_token) = match dht {
                Some((d, t)) => (Some(d), Some(t)),
                None => (None, None),
            };
            let (lsd, lsd_token) = match lsd {
                Some((l, t)) => (Some(Arc::new(l)), Some(t)),
                None => (None, None),
            };

            let session = Arc::new(Self {
                persistence,
                bitv_factory,
                peer_id,
                anon_peer_id,
                listen: Default::default(),
                listen_lock: Default::default(),
                incoming_seen: Default::default(),
                dht: ArcSwapOption::new(dht),
                dht_ctl: parking_lot::Mutex::new(networking::ServiceCtl {
                    config: dht_restart,
                    token: dht_token,
                }),
                discovery_gen: tokio::sync::watch::Sender::new(0),
                bind_device: RwLock::new(bind_device.clone()),
                http_env,
                peer_filter: PeerFilter::default(),
                req2_index: Req2Index::default(),
                connect_limiter: Arc::new(ConnectRateLimiter::new(net.connection_speed)),
                port_mapper: PortMapper::new(
                    token.child_token(),
                    port_mapping,
                    external_ip.clone(),
                ),
                external_ip,
                i2p: Default::default(),
                peer_opts,
                spawner: spawner.clone(),
                output_folder: default_output_folder,
                next_id: AtomicUsize::new(0),
                db: RwLock::new(Default::default()),
                _cancellation_token_drop_guard: token.clone().drop_guard(),
                cancellation_token: token,
                default_storage_factory: opts.default_storage_factory,
                reqwest_client,
                fetch_budget: crate::peer_info_reader::budget::FetchBudget::new(
                    crate::peer_info_reader::budget::DEFAULT_FETCH_BUDGET,
                ),
                webseed_http: arc_swap::ArcSwap::from_pointee(webseed_http),
                webseed_fetches: Arc::new(tokio::sync::Semaphore::new(MAX_WEBSEED_FETCHES)),
                connector: stream_connector,
                root_span: opts.root_span,
                stats: Arc::new(SessionStats::new()),
                checking_limiter: CheckingLimiter::new(Some(
                    opts.concurrent_init_limit.unwrap_or(3),
                )),
                tracker_ctx,
                storage: StorageContext::new(opts.storage.clone()),
                ratelimits: Limits::new(opts.ratelimits),
                peer_policy: SessionPeerPolicy::new(opts.peer_policy),
                ipv4_only: opts.ipv4_only,
                trackers: opts.trackers,
                peer_limit: opts.peer_limit,
                client_name_and_version,

                #[cfg(feature = "disable-upload")]
                _disable_upload: opts.disable_upload,
                blocklist,
                allowlist,
                lsd: ArcSwapOption::new(lsd),
                lsd_token: parking_lot::Mutex::new(lsd_token),
            });

            session.port_mapper.set_bind_device(bind_device.clone());
            if let Some(listen_opts) = listen_opts {
                session
                    .set_listen_options(Some(listen_opts))
                    .await
                    .context("error starting listeners")?;
            }
            if let Some(i2p) = opts.i2p.take() {
                session.set_i2p(Some(i2p));
            }

            if let Some(persistence) = session.persistence.as_ref() {
                info!("will use {persistence:?} for session persistence");

                let mut ps = persistence.stream_all().await?;
                let mut added_all = false;
                let mut futs = FuturesUnordered::new();

                while !added_all || !futs.is_empty() {
                    // NOTE: this closure exists purely to workaround rustfmt screwing up when inlining it.
                    let add_torrent_span = |info_hash: &Id20| -> tracing::Span {
                        debug_span!(parent: session.rs(), "add_torrent", info_hash=?info_hash)
                    };
                    tokio::select! {
                        Some(res) = futs.next(), if !futs.is_empty() => {
                            if let Err(e) = res {
                                error!("error adding torrent to session: {e:#}");
                            }
                        }
                        st = ps.next(), if !added_all => {
                            match st {
                                Some(st) => {
                                    let (id, st) = st?;
                                    let span = add_torrent_span(st.info_hash());
                                    let (add_torrent, mut opts) = st.into_add_torrent()?;
                                    opts.preferred_id = Some(id);
                                    let fut = session.add_torrent(add_torrent, Some(opts));
                                    let fut = fut.instrument(span);
                                    futs.push(fut);
                                },
                                None => added_all = true
                            };
                        }
                    };
                }
            }

            session.start_peer_tick();

            Ok(session)
        }
        .boxed()
    }

    pub(crate) async fn check_incoming_connection(
        self: Arc<Self>,
        addr: SocketAddr,
        kind: ConnectionKind,
        reader: BoxAsyncReadVectored,
        writer: BoxAsyncWrite,
    ) -> anyhow::Result<(Arc<TorrentStateLive>, CheckedIncomingConnection)> {
        let rwtimeout = self
            .peer_opts
            .read_write_timeout
            .unwrap_or_else(|| Duration::from_secs(10));

        let incoming_ip = crate::net::filter::canonical_ip(addr.ip());
        if self.blocklist.has(incoming_ip) {
            self.stats
                .counters
                .blocked_incoming
                .fetch_add(1, Ordering::Relaxed);
            bail!("Incoming ip {incoming_ip} is in blocklist");
        }
        if self.allowlist.as_ref().is_some_and(|l| !l.has(incoming_ip)) {
            self.stats
                .counters
                .blocked_incoming
                .fetch_add(1, Ordering::Relaxed);
            bail!("Incoming ip {incoming_ip} is not in allowlist");
        }

        // Protocol encryption: detect plaintext vs MSE and run the receiver
        // side of the encrypted handshake.
        let policy = self.connector.settings().encryption;
        let (mut reader, writer, encrypted, mse_info_hash) =
            if policy == crate::net::EncryptionPolicy::Disabled {
                (reader, writer, false, None)
            } else {
                let accepted = tokio::time::timeout(
                    rwtimeout,
                    librqbit_mse::accept(
                        Duplex {
                            read: reader,
                            write: writer,
                        },
                        policy,
                        |req2| self.req2_index.lookup(req2),
                    ),
                )
                .await
                .context("timeout in encryption handshake")?
                .context("encryption handshake")?;
                match accepted {
                    librqbit_mse::Accepted::Plaintext(s) => {
                        let (r, w, _) = split_mse(s);
                        (r, w, false, None)
                    }
                    librqbit_mse::Accepted::Encrypted { stream, info_hash } => {
                        let (r, w, enc) = split_mse(stream);
                        (r, w, enc, Some(info_hash))
                    }
                }
            };

        let mut read_buf = ReadBuf::new();
        let h = read_buf
            .read_handshake(&mut reader, rwtimeout)
            .await
            .context("error reading handshake")?;
        trace!("received handshake from {addr}: {:?}", h);

        if mse_info_hash.is_some_and(|ih| ih != h.info_hash.0) {
            bail!("info hash of the encrypted handshake doesn't match the BitTorrent handshake");
        }

        if self.is_own_peer_id(h.peer_id) {
            bail!("seems like we are connecting to ourselves, ignoring");
        }

        let (id, torrent) = self
            .db
            .read()
            .torrents
            .iter()
            .find(|(_, t)| t.has_info_hash(h.info_hash))
            .map(|(id, t)| (*id, t.clone()))
            .with_context(|| format!("didn't find a matching torrent {:?}", h.info_hash))?;

        if !self.i2p_mixed_allows(&torrent, addr) {
            self.peer_filter
                .report(addr, crate::net::PeerBlockReason::I2pMixedMode, true);
            bail!("non-I2P peer of an I2P torrent refused (mixed mode off)");
        }

        let live = torrent
            .live_wait_initializing(Duration::from_secs(5))
            .await
            .with_context(|| format!("torrent {id} is not live, ignoring connection"))?;

        Ok((
            live,
            CheckedIncomingConnection {
                addr,
                reader,
                writer,
                kind,
                encrypted,
                handshake: h,
                read_buf,
            },
        ))
    }

    pub(crate) async fn task_listener<A: Accept>(
        self: Arc<Self>,
        l: A,
        max_pending_incoming_handshake_checks: usize,
    ) -> anyhow::Result<()> {
        let mut futs = FuturesUnordered::new();
        // Pending handshakes per address (IPv6: per /64), so that one host
        // holding slow handshakes can't take every slot.
        let mut pending: HashMap<IpAddr, usize> = HashMap::new();
        // After an accept error (e.g. out of file descriptors) accepting
        // pauses, but pending handshakes keep being served.
        let mut accept_paused_until: Option<tokio::time::Instant> = None;
        let mut tuning = StreamConnector::tuning(&self.connector.settings());
        let session = Arc::downgrade(&self);
        drop(self);

        loop {
            let accepting = accept_paused_until.is_none()
                && futs.len() < max_pending_incoming_handshake_checks;
            tokio::select! {
                r = l.accept(tuning), if accepting => {
                    match r {
                        Ok((addr, (read, write))) => {
                            trace!("accepted connection from {addr}");
                            let session = session.upgrade().context("session is dead")?;
                            tuning = StreamConnector::tuning(&session.connector.settings());
                            if !session.accept_incoming_allowed(addr) {
                                continue;
                            }
                            let key = pending_handshake_key(addr.ip());
                            let n = pending.entry(key).or_default();
                            if *n >= MAX_PENDING_HANDSHAKES_PER_IP {
                                debug!(?addr, "too many pending handshakes from this address");
                                continue;
                            }
                            *n += 1;
                            let span = debug_span!(parent: session.rs(), "incoming", addr=%addr);
                            let check = session.check_incoming_connection(addr, A::KIND, Box::new(read), Box::new(write));
                            futs.push(
                                async move {
                                    let res = match tokio::time::timeout(INCOMING_HANDSHAKE_DEADLINE, check).await {
                                        Ok(r) => r,
                                        Err(_) => Err(anyhow::anyhow!("incoming handshake took too long")),
                                    };
                                    if let Err(e) = &res {
                                        debug!("error checking incoming connection: {e:#}");
                                    }
                                    (key, res)
                                }
                                .instrument(span)
                            );
                        }
                        Err(e) => {
                            warn!("error accepting: {e:#}");
                            // Whatever is the reason, ensure we are not stuck trying to
                            // accept indefinitely.
                            accept_paused_until = Some(tokio::time::Instant::now() + Duration::from_secs(10));
                            continue
                        }
                    }
                },
                _ = tokio::time::sleep_until(accept_paused_until.unwrap_or_else(tokio::time::Instant::now)), if accept_paused_until.is_some() => {
                    accept_paused_until = None;
                },
                Some((key, res)) = futs.next(), if !futs.is_empty() => {
                    if let Some(n) = pending.get_mut(&key) {
                        *n = n.saturating_sub(1);
                        if *n == 0 {
                            pending.remove(&key);
                        }
                    }
                    let Ok((live, checked)) = res else { continue };
                    let (addr, kind) = (checked.addr, checked.kind);
                    // The filter or the bans may have changed during the handshake.
                    if let Some(session) = session.upgrade()
                        && session.peer_filter.check_and_report(addr, true)
                    {
                        continue;
                    }
                    match live.add_incoming_peer(checked) {
                        Ok(_) => {
                            if let Some(session) = session.upgrade() {
                                session.incoming_seen.store(true, Ordering::Relaxed);
                            }
                        }
                        Err(e) => warn!(?addr, ?kind, "error handing over incoming connection: {e:#}"),
                    }
                },
                else => continue,
            }
        }
    }

    /// The HTTP client web seeds use (`trusted`: added by the user or from
    /// a local torrent).
    pub(crate) fn webseed_http_client(&self, trusted: bool) -> reqwest::Client {
        let c = self.webseed_http.load();
        if trusted {
            c.trusted.clone()
        } else {
            c.untrusted.clone()
        }
    }

    /// The DHT, if enabled (see `Session::set_dht_enabled`).
    pub fn get_dht(&self) -> Option<Dht> {
        self.dht.load_full()
    }

    fn merge_peer_opts(&self, other: Option<PeerConnectionOptions>) -> PeerConnectionOptions {
        let other = match other {
            Some(o) => o,
            None => self.peer_opts,
        };
        PeerConnectionOptions {
            connect_timeout: other.connect_timeout.or(self.peer_opts.connect_timeout),
            read_write_timeout: other
                .read_write_timeout
                .or(self.peer_opts.read_write_timeout),
            keep_alive_interval: other
                .keep_alive_interval
                .or(self.peer_opts.keep_alive_interval),
            max_request_window: other
                .max_request_window
                .or(self.peer_opts.max_request_window),
        }
    }

    /// Spawn a task in the context of the session.
    #[track_caller]
    pub fn spawn(
        &self,
        span: tracing::Span,
        name: impl Into<Cow<'static, str>>,
        fut: impl std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    ) {
        spawn_with_cancel(span, name, self.cancellation_token.clone(), fut);
    }

    pub(crate) fn rs(&self) -> Option<tracing::Id> {
        self.root_span.as_ref().and_then(|s| s.id())
    }

    /// Stop the session and all managed tasks.
    pub async fn stop(&self) {
        let torrents = self
            .db
            .read()
            .torrents
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut paused = Vec::new();
        for torrent in torrents {
            match torrent.pause() {
                Ok(()) => paused.push(torrent),
                Err(e) => debug!("error pausing torrent: {e:#}"),
            }
        }
        // Torrents paused before were recorded then.
        for torrent in paused {
            torrent.save_resume_stats().await;
        }
        // Give trackers their `stopped` announces.
        let stop_timeout = self.tracker_ctx.settings().stop_tracker_timeout;
        if !stop_timeout.is_zero() {
            self.tracker_ctx
                .active
                .wait_idle(Instant::now() + stop_timeout + Duration::from_millis(500))
                .await;
        }
        self.cancellation_token.cancel();
        // this sucks, but hopefully will be enough
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    /// Run a callback given the currently managed torrents.
    pub fn with_torrents<R>(
        &self,
        callback: impl Fn(&mut dyn Iterator<Item = (TorrentId, &ManagedTorrentHandle)>) -> R,
    ) -> R {
        callback(&mut self.db.read().torrents.iter().map(|(id, t)| (*id, t)))
    }

    /// Add a torrent to the session.
    #[inline(never)]
    pub fn add_torrent<'a>(
        self: &'a Arc<Self>,
        add: AddTorrent<'a>,
        opts: Option<AddTorrentOptions>,
    ) -> BoxFuture<'a, anyhow::Result<AddTorrentResponse>> {
        async move {
            let mut opts = opts.unwrap_or_default();
            let add_res = match add {
                AddTorrent::Url(magnet)
                    if magnet.starts_with("magnet:") || is_bare_info_hash(&magnet) =>
                {
                    let web_seeds = web_seeds_from_magnet(&magnet);
                    let magnet = Magnet::parse(&magnet)
                        .context("provided path is not a valid magnet URL")?;
                    // A v2-only magnet is identified by its truncated
                    // SHA-256 hash (BEP 52); with both, the v1 hash wins.
                    let info_hash_v2 = magnet.as_id20().is_none().then(|| magnet.as_id32()).flatten();
                    let info_hash = magnet
                        .as_id20()
                        .or(info_hash_v2.map(|h| h.truncate_for_dht()))
                        .context("magnet link has no info hash")?;
                    if let Some(so) = magnet.get_select_only() {
                        // Only overwrite opts.only_files if user didn't specify
                        if opts.only_files.is_none() {
                            opts.only_files = Some(so);
                        }
                    }

                    InternalAddResult {
                        info_hash,
                        info_hash_v2,
                        // Like libtorrent: every magnet tracker gets its own tier.
                        trackers: magnet
                            .trackers
                            .into_iter()
                            .enumerate()
                            .map(|(tier, url)| (url, u32::try_from(tier).unwrap_or(u32::MAX)))
                            .collect(),
                        local_source: false,
                        metadata: None,
                        name: magnet.name,
                        web_seeds,
                    }
                }
                other => {
                    let local_source = matches!(other, AddTorrent::TorrentFileBytes(_));
                    let torrent = match other {
                        AddTorrent::Url(url)
                            if url.starts_with("http://") || url.starts_with("https://") =>
                        {
                            torrent_from_url(&self.reqwest_client, &url).await?
                        }
                        AddTorrent::Url(url) => {
                            bail!(
                                "unsupported URL {:?}. Supporting magnet:, http:, and https",
                                url
                            )
                        }
                        AddTorrent::TorrentFileBytes(bytes) => {
                            torrent_from_bytes(bytes).context("error decoding torrent")?
                        }
                    };

                    let trackers = metainfo_trackers(&torrent.meta);
                    let layers = torrent.meta.piece_layers.as_ref().map(|l| l.0.as_ref());
                    // Like libtorrent, a pure v2 .torrent may lack piece
                    // layers (those present must be valid): it starts at
                    // once, and the files without one wait for theirs,
                    // fetched from peers (see `missing_layers`).
                    let info = torrent
                        .meta
                        .info
                        .data
                        .validate()?
                        .with_partial_piece_layers(layers)?;
                    let metadata = Some(TorrentMetadata::new(
                        info,
                        torrent.torrent_bytes,
                        torrent.meta.info.raw_bytes.0,
                    )?);

                    InternalAddResult {
                        info_hash: torrent.meta.info_hash,
                        info_hash_v2: None,
                        metadata,
                        trackers,
                        local_source,
                        name: None,
                        web_seeds: Vec::new(),
                    }
                }
            };

            self.add_torrent_internal(add_res, opts).await
        }
        .instrument(debug_span!(parent: self.rs(), "add_torrent"))
        .boxed()
    }

    fn get_default_subfolder_for_torrent(
        &self,
        info: &ValidatedTorrentMetaV1Info<ByteBufOwned>,
        magnet_name: Option<&str>,
    ) -> anyhow::Result<Option<PathBuf>> {
        let files = info
            .iter_file_details()
            .map(|fd| Ok((fd.filename.to_pathbuf(), fd.len)))
            .collect::<anyhow::Result<Vec<(PathBuf, u64)>>>()?;
        if files.len() < 2 {
            return Ok(None);
        }

        fn check_valid(pb: &Path) -> anyhow::Result<()> {
            if pb.components().any(|x| !matches!(x, Component::Normal(_))) {
                bail!("path traversal in torrent name detected")
            }
            Ok(())
        }

        if let Some(name) = info.name()
            && !name.is_empty()
        {
            // A single, sanitized component: the name can't add folders or escape.
            let pb = PathBuf::from(&*sanitize_component(&name));
            check_valid(&pb)?;
            return Ok(Some(pb));
        };
        if let Some(name) = magnet_name {
            let pb = PathBuf::from(&*sanitize_component(name));
            check_valid(&pb)?;
            return Ok(Some(pb));
        }
        // Let the subfolder name be the longest filename
        let longest = files
            .iter()
            .max_by_key(|(_, l)| l)
            .unwrap()
            .0
            .file_stem()
            .context("can't determine longest filename")?;
        let pb = PathBuf::from(&*sanitize_component(&longest.to_string_lossy()));
        check_valid(&pb)?;
        Ok::<_, anyhow::Error>(Some(pb))
    }

    async fn add_torrent_internal(
        self: &Arc<Self>,
        add_res: InternalAddResult,
        mut opts: AddTorrentOptions,
    ) -> anyhow::Result<AddTorrentResponse> {
        let InternalAddResult {
            info_hash,
            info_hash_v2,
            metadata,
            trackers,
            local_source,
            name,
            web_seeds: magnet_web_seeds,
        } = add_res;

        if !opts.list_only
            && opts.preferred_id.is_none()
            && let Some(existing) = self.get(TorrentIdOrHash::Hash(info_hash))
        {
            return Ok(AddTorrentResponse::AlreadyManaged(existing.id(), existing));
        }

        let private = metadata.as_ref().is_some_and(|m| m.info.info().private);
        let local_source = opts.trackers_local_source.unwrap_or(local_source);
        let tracker_manager = TorrentTrackers::new(
            info_hash,
            initial_tracker_list(&trackers, &mut opts, private, local_source, &self.trackers),
            private,
            local_source,
            opts.disable_trackers,
            opts.force_tracker_interval,
        );
        tracker_manager.set_disabled_discovery(opts.disabled_discovery);
        if let Some(out) = &opts.tracker_manager_out {
            let _ = out.set(tracker_manager.clone());
        }
        if info_hash_v2.is_some() {
            // A `btmh` magnet: discovered under a v2 hash.
            tracker_manager.set_announces_v2();
        }

        let known_alt = metadata.as_ref().and_then(hybrid_hashes).map(|(_, alt)| alt);
        let make_peer_rx = || {
            self.make_peer_rx(
                &tracker_manager,
                info_hash,
                known_alt,
                !opts.paused && !opts.list_only,
                opts.initial_peers.clone().unwrap_or_default(),
                private,
            )
        };

        let mut seen_peers = Vec::new();

        let (metadata, peer_rx) = {
            match metadata {
                Some(metadata) => {
                    let mut peer_rx = None;
                    if !opts.paused && !opts.list_only {
                        peer_rx = make_peer_rx();
                    }
                    (metadata, peer_rx)
                }
                None => {
                    if self.dht.load().is_none()
                        && self.lsd.load().is_none()
                        && opts.initial_peers.as_ref().is_none_or(|p| p.is_empty())
                        && (tracker_manager.is_empty()
                            || self.tracker_ctx.disabled
                            || tracker_manager.is_disabled())
                    {
                        bail!(
                            "no known way to resolve peers (no DHT, no trackers, no initial_peers)"
                        );
                    }
                    let peer_rx = make_peer_rx().context(
                        "no known way to resolve peers (no DHT, no trackers, no initial_peers)",
                    )?;
                    let resolved_magnet = self
                        .resolve_magnet(
                            info_hash,
                            info_hash_v2,
                            peer_rx,
                            &tracker_manager.entries(),
                            opts.peer_opts,
                        )
                        .await?;
                    if resolved_magnet.metadata.info.info().private {
                        // No DHT/LSD and no session-wide trackers for private torrents.
                        tracker_manager.set_private();
                    }

                    // Add back seen_peers into the peer stream, as we consumed some peers
                    // while resolving the magnet.
                    seen_peers = resolved_magnet.seen_peers.clone();
                    let mut peer_rx = merge_streams(
                        resolved_magnet.peer_rx,
                        futures::stream::iter(resolved_magnet.seen_peers),
                    )
                    .boxed();
                    // A hybrid torrent: DHT and LSD under its other hash too
                    // (trackers follow the torrent by themselves).
                    if !opts.list_only
                        && let Some((v1, alt)) = hybrid_hashes(&resolved_magnet.metadata)
                    {
                        let other = if info_hash == v1 { alt } else { v1 };
                        let private = resolved_magnet.metadata.info.info().private;
                        if let Some(extra) = self.discovery_rx(
                            &tracker_manager,
                            other,
                            other == alt,
                            !opts.paused,
                            private,
                        ) {
                            peer_rx = merge_streams(peer_rx, extra).boxed();
                        }
                    }
                    (resolved_magnet.metadata, Some(peer_rx))
                }
            }
        };

        trace!("Torrent metadata: {:#?}", &metadata.info.info());

        // A hybrid torrent runs on its v1 hash (also when it was added by a
        // `btmh` magnet) and is reachable under its truncated v2 hash too.
        let (info_hash, info_hash_alt) = match hybrid_hashes(&metadata) {
            Some((v1, alt)) => (v1, Some(alt)),
            None => (info_hash, None),
        };

        let only_files = compute_only_files(
            &metadata.info,
            opts.only_files,
            opts.only_files_regex,
            opts.list_only,
        )?;

        let (output_folder, derived_folder_is_own) = match (opts.output_folder, opts.sub_folder) {
            (None, None) => {
                let sub =
                    self.get_default_subfolder_for_torrent(&metadata.info, name.as_deref())?;
                let own = sub.is_some();
                (self.output_folder.join(sub.unwrap_or_default()), own)
            }
            (Some(o), None) => (PathBuf::from(o), false),
            (Some(_), Some(_)) => {
                bail!("you can't provide both output_folder and sub_folder")
            }
            (None, Some(s)) => {
                let own = !s.is_empty();
                (self.output_folder.join(s), own)
            }
        };

        if opts.list_only {
            return Ok(AddTorrentResponse::ListOnly(ListOnlyResponse {
                info_hash,
                info: metadata.info,
                only_files,
                output_folder,
                seen_peers,
                torrent_bytes: metadata.torrent_bytes,
            }));
        }

        let web_seeds = Arc::new(match opts.replace_web_seeds.take() {
            Some(list) if local_source => TorrentWebSeeds::new(Vec::new(), list),
            Some(list) => {
                let trusted_urls = opts.trusted_urls.take().unwrap_or_default();
                TorrentWebSeeds::with_trust(
                    list.into_iter()
                        .map(|u| {
                            let trusted = trusted_urls.iter().any(|t| t.trim() == u.trim());
                            (u, trusted)
                        })
                        .collect(),
                )
            }
            None => {
                let mut torrent_seeds = url_list_from_torrent(&metadata.torrent_bytes);
                torrent_seeds.extend(magnet_web_seeds);
                TorrentWebSeeds::new(torrent_seeds, opts.web_seeds.take().unwrap_or_default())
            }
        });

        let storage_factory = opts
            .storage_factory
            .take()
            .or_else(|| self.default_storage_factory.as_ref().map(|f| f.clone_box()))
            .unwrap_or_else(|| FilesystemStorageFactory::default().boxed());

        let layout = Arc::new(StorageLayout::new(
            self.storage.clone(),
            storage_factory.is_type_id(std::any::TypeId::of::<FilesystemStorageFactory>()),
            opts.owns_output_folder.unwrap_or(derived_folder_is_own),
            output_folder,
            metadata.lengths(),
            &metadata.file_infos,
            opts.renamed_files.as_ref(),
            only_files.as_deref(),
        )?);

        let id = if let Some(id) = opts.preferred_id {
            id
        } else if let Some(p) = self.persistence.as_ref() {
            p.next_id().await?
        } else {
            self.next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        };

        let _permit = self.spawner.semaphore().acquire_owned().await?;

        let (managed_torrent, metadata) = {
            let mut g = self.db.write();
            if let Some((id, handle)) = g.torrents.iter().find_map(|(eid, t)| {
                if t.has_info_hash(info_hash)
                    || info_hash_alt.is_some_and(|a| t.has_info_hash(a))
                    || *eid == id
                {
                    Some((*eid, t.clone()))
                } else {
                    None
                }
            }) {
                return Ok(AddTorrentResponse::AlreadyManaged(id, handle));
            }

            let span = debug_span!(parent: self.rs(), "torrent", id);
            let peer_opts = self.merge_peer_opts(opts.peer_opts);
            let metadata = Arc::new(metadata);
            let minfo = Arc::new(ManagedTorrentShared {
                id,
                span,
                info_hash,
                info_hash_alt,
                trackers: tracker_manager
                    .entries()
                    .into_iter()
                    .filter_map(|(u, _)| url::Url::parse(&u).ok())
                    .collect(),
                tracker_manager: tracker_manager.clone(),
                web_seeds,
                spawner: self.spawner.clone(),
                peer_id: self.peer_id,
                storage_factory,
                layout,
                options: ManagedTorrentOptions {
                    peer_connect_timeout: peer_opts.connect_timeout,
                    peer_read_write_timeout: peer_opts.read_write_timeout,
                    peer_max_request_window: peer_opts.max_request_window,
                    allow_overwrite: opts.overwrite,
                    skip_hash_check: opts.skip_hash_check,
                    initial_peers: opts.initial_peers.clone().unwrap_or_default(),
                    #[cfg(feature = "disable-upload")]
                    _disable_upload: self._disable_upload,
                },
                connector: self.connector.clone(),
                session: Arc::downgrade(self),
                magnet_name: name,
                client_name_and_version: self.client_name_and_version.clone(),
                runtime: TorrentRuntime::new(
                    opts.ratelimits,
                    self.peer_policy.default_max_uploads_per_torrent(),
                    Some(
                        opts.peer_limit
                            .or(self.peer_limit)
                            .unwrap_or(DEFAULT_PEER_LIMIT_PER_TORRENT),
                    ),
                ),
                v2_fetched_layers: Default::default(),
                fetch_budget: self.fetch_budget.clone(),
                v2_layers_unsaved: Default::default(),
                v2_unverified: Default::default(),
            });

            let initializing = Arc::new(TorrentStateInitializing::new(
                minfo.clone(),
                metadata.clone(),
                only_files.clone(),
                self.spawner
                    .block_in_place(|| minfo.storage_factory.create_and_init(&minfo, &metadata))?,
                false,
            ));
            let handle = Arc::new(ManagedTorrent {
                locked: RwLock::new(ManagedTorrentLocked {
                    paused: opts.paused,
                    state: ManagedTorrentState::Initializing(initializing),
                    file_priorities: file_priorities_from_only_files(
                        metadata.file_infos.len(),
                        only_files.as_deref(),
                    ),
                    only_files,
                }),
                state_change_notify: Notify::new(),
                shared: minfo,
                metadata: ArcSwapOption::new(Some(metadata.clone())),
            });

            g.add_torrent(handle.clone(), id);
            self.req2_index.insert(info_hash);
            if let Some(alt) = info_hash_alt {
                self.req2_index.insert(alt);
            }
            tracker_manager.set_torrent(&handle);
            (handle, metadata)
        };

        if let Some(p) = self.persistence.as_ref()
            && let Err(e) = p.store(id, &managed_torrent).await
        {
            self.db.write().torrents.remove(&id);
            self.req2_index.remove(info_hash);
            if let Some(alt) = info_hash_alt {
                self.req2_index.remove(alt);
            }
            return Err(e);
        }

        let _e = managed_torrent.shared.span.clone().entered();

        managed_torrent
            .start(peer_rx, opts.paused)
            .context("error starting torrent")?;

        if let Some(name) = metadata.info.name() {
            info!(?name, "added torrent");
        }

        Ok(AddTorrentResponse::Added(id, managed_torrent))
    }

    pub fn get(&self, id: TorrentIdOrHash) -> Option<ManagedTorrentHandle> {
        match id {
            TorrentIdOrHash::Id(id) => self.db.read().torrents.get(&id).cloned(),
            TorrentIdOrHash::Hash(id) => self.db.read().torrents.iter().find_map(|(_, v)| {
                if v.has_info_hash(id) {
                    Some(v.clone())
                } else {
                    None
                }
            }),
        }
    }

    /// Remove a torrent, optionally deleting its files. See [`Session::delete_with_options`].
    pub async fn delete(&self, id: TorrentIdOrHash, delete_files: bool) -> anyhow::Result<()> {
        self.delete_with_options(
            id,
            DeleteOptions {
                delete_files,
                trash: None,
            },
        )
        .await
    }

    /// Remove a torrent. With `delete_files`, only the torrent's own files are
    /// deleted (at their current paths, renames and decorations included), then the
    /// folders they leave empty, and the output folder if the torrent owns it.
    /// A running storage move is waited for first.
    pub async fn delete_with_options(
        &self,
        id: TorrentIdOrHash,
        opts: DeleteOptions,
    ) -> anyhow::Result<()> {
        let id = match id {
            TorrentIdOrHash::Id(id) => id,
            TorrentIdOrHash::Hash(h) => self
                .db
                .read()
                .torrents
                .values()
                .find_map(|v| {
                    if v.has_info_hash(h) {
                        Some(v.id())
                    } else {
                        None
                    }
                })
                .context("no such torrent in db")?,
        };
        let removed = self
            .db
            .write()
            .torrents
            .remove(&id)
            .with_context(|| format!("torrent with id {id} did not exist"))?;
        self.req2_index.remove(removed.info_hash());
        if let Some(alt) = removed.info_hash_alt() {
            self.req2_index.remove(alt);
        }

        let layout = removed.shared.layout.clone();
        let _op = layout.op_lock().lock().await;

        if let Err(e) = removed.pause() {
            debug!("error pausing torrent before deletion: {e:#}")
        }

        let metadata = removed.metadata.load_full().expect("TODO");

        let storage = removed
            .with_state_mut(|s| match s.take() {
                ManagedTorrentState::Initializing(p) => p.files.take().ok(),
                ManagedTorrentState::Paused(p) => Some(p.files),
                ManagedTorrentState::Live(l) => l
                    .pause()
                    // inspect_err not available in 1.75
                    .map_err(|e| {
                        warn!(?id, "error pausing torrent: {e:#}");
                        e
                    })
                    .ok()
                    .map(|p| p.files),
                _ => None,
            })
            .map(Ok)
            .unwrap_or_else(|| {
                removed
                    .shared
                    .storage_factory
                    .create(removed.shared(), &metadata)
            });

        if let Some(p) = self.persistence.as_ref() {
            if let Err(e) = p.delete(id).await {
                error!(
                    ?id,
                    "error deleting torrent from persistence database: {e:#}"
                );
            } else {
                debug!(?id, "deleted torrent from persistence database")
            }
        }

        if opts.delete_files {
            debug!("will delete files");
            if layout.is_disk_backed() {
                drop(storage);
                let trash = opts
                    .trash
                    .as_deref()
                    .map(|t| t as &dyn Fn(&Path) -> std::io::Result<()>);
                self.spawner
                    .block_in_place(|| removed.delete_files_from_disk(trash));
            } else {
                let storage = storage.context("torrent deleted, but could not delete files")?;
                remove_files_and_dirs(&metadata.file_infos, &storage);
            }
        } else {
            debug!("not deleting files")
        }

        info!(id, "deleted torrent");
        Ok(())
    }

    /// Current storage settings.
    pub fn storage_settings(&self) -> StorageSettings {
        (*self.storage.settings()).clone()
    }

    /// Change storage settings at runtime. When the incomplete suffix or the
    /// unwanted folder option changes, every torrent's files are renamed
    /// accordingly; the file handle pool is resized; new cache modes apply to files
    /// opened from now on (all handles are closed).
    pub async fn set_storage_settings(&self, settings: StorageSettings) -> anyhow::Result<()> {
        settings.validate()?;
        let change = self.storage.set_settings(settings);
        if change.layout {
            let torrents: Vec<ManagedTorrentHandle> =
                self.db.read().torrents.values().cloned().collect();
            for t in torrents {
                let layout = t.shared.layout.clone();
                self.spawner
                    .block_in_place_with_semaphore(|| layout.reconcile_all())
                    .await;
            }
        }
        Ok(())
    }

    /// Maximum number of torrents checking their data at the same time; negative =
    /// unlimited.
    pub fn set_max_active_checking_torrents(&self, limit: i64) {
        self.checking_limiter.set_limit(usize::try_from(limit).ok());
    }

    /// See [`Session::set_max_active_checking_torrents`]; -1 = unlimited.
    pub fn max_active_checking_torrents(&self) -> i64 {
        self.checking_limiter
            .limit()
            .and_then(|l| i64::try_from(l).ok())
            .unwrap_or(-1)
    }

    pub fn make_peer_rx_managed_torrent(
        self: &Arc<Self>,
        t: &Arc<ManagedTorrent>,
        announce: bool,
    ) -> Option<PeerStream> {
        let tm = t.shared().tracker_manager.clone();
        if t.live().is_none() {
            // A new run of the torrent: forget the previous run's peers.
            tm.peer_sources.clear();
        }
        let is_private =
            tm.is_private() || t.with_metadata(|m| m.info.info().private).unwrap_or(false);
        self.make_peer_rx(
            &tm,
            t.info_hash(),
            t.info_hash_alt(),
            announce,
            t.shared().options.initial_peers.clone(),
            is_private,
        )
    }

    /// DHT and LSD peers under one hash of a torrent. `v2`: a (truncated)
    /// v2 hash; peers found under it are connected to with that hash.
    fn discovery_sourced(
        self: &Arc<Self>,
        tm: &Arc<TorrentTrackers>,
        info_hash: Id20,
        v2: bool,
        announce: bool,
        is_private: bool,
    ) -> Option<SourcedPeerStream> {
        // DHT and LSD stop as soon as the torrent turns out to be private.
        // They follow the session: switching DHT/LSD on or off, or a new
        // listen port, restarts these streams. I2P torrents without mixed
        // mode don't use them either.
        if is_private || self.i2p.i2p_only(tm) {
            return None;
        }
        let one = |source| {
            self.discovery_stream(
                info_hash,
                announce,
                source,
                tm.watch_disabled_discovery(),
                tm.dht_pacing.pacer(),
            )
                .take_until(tm.clone().wait_private())
                .boxed()
        };
        let both = merge_streams(one(PeerSource::Dht), one(PeerSource::Lsd));
        Some(if v2 {
            let tm = tm.clone();
            both.map(move |(addr, source)| {
                tm.peer_sources.mark_v2(addr);
                (addr, source)
            })
            .boxed()
        } else {
            both.boxed()
        })
    }

    /// [`Self::discovery_sourced`] as a plain peer stream.
    fn discovery_rx(
        self: &Arc<Self>,
        tm: &Arc<TorrentTrackers>,
        info_hash: Id20,
        v2: bool,
        announce: bool,
        is_private: bool,
    ) -> Option<PeerStream> {
        self.discovery_sourced(tm, info_hash, v2, announce, is_private)
            .map(|s| record_and_strip(s, tm.clone()))
    }

    // Get a peer stream from DHT, LSD, trackers and initial peers.
    // `info_hash_alt`: the truncated v2 hash of a hybrid torrent (DHT and LSD
    // run for both hashes like in libtorrent; the tracker announcer covers
    // both by itself).
    fn make_peer_rx(
        self: &Arc<Self>,
        tm: &Arc<TorrentTrackers>,
        info_hash: Id20,
        info_hash_alt: Option<Id20>,
        announce: bool,
        initial_peers: Vec<SocketAddr>,
        is_private: bool,
    ) -> Option<PeerStream> {
        let dht_rx = merge_two_optional_streams(
            self.discovery_sourced(tm, info_hash, tm.is_v2_hash(info_hash), announce, is_private),
            info_hash_alt
                .and_then(|alt| self.discovery_sourced(tm, alt, true, announce, is_private)),
        );

        let tracker_rx = if self.tracker_ctx.disabled || tm.is_disabled() {
            None
        } else {
            Some(tm.start(self.tracker_ctx.clone()))
        };

        let initial_peers_rx: Option<SourcedPeerStream> = if initial_peers.is_empty() {
            None
        } else {
            Some(
                futures::stream::iter(initial_peers)
                    .map(|a| (a, PeerSource::Manual))
                    .boxed(),
            )
        };
        let merged = merge_two_optional_streams(
            merge_two_optional_streams(dht_rx, tracker_rx),
            initial_peers_rx,
        )?;
        Some(record_and_strip(merged, tm.clone()))
    }

    /// Current tracker settings.
    pub fn tracker_settings(&self) -> TrackerSettings {
        self.tracker_ctx.settings()
    }

    /// Change tracker settings at runtime. They apply to the next announce of
    /// every torrent.
    pub fn set_tracker_settings(&self, settings: TrackerSettings) -> anyhow::Result<()> {
        self.tracker_ctx.set_settings(settings)?;
        self.with_torrents(|torrents| {
            for (_, t) in torrents {
                t.shared().tracker_manager.wake();
            }
        });
        Ok(())
    }

    /// Set (or clear) a predicate that returns true for addresses blocked by
    /// the IP filter; trackers on such addresses are not contacted ("Apply to
    /// trackers").
    pub fn set_tracker_ip_filter(&self, filter: Option<IpFilterFn>) {
        self.tracker_ctx.client.policy().set_ip_filter(filter);
    }

    /// Announce every started torrent to all its trackers now, ignoring the
    /// minimum interval (e.g. after an IP change or a system wake-up).
    pub fn reannounce_all(&self) {
        self.with_torrents(|torrents| {
            for (_, t) in torrents {
                t.force_reannounce(None);
            }
        });
    }

    pub(crate) async fn try_update_persistence_metadata(&self, handle: &ManagedTorrentHandle) {
        if let Some(p) = self.persistence.as_ref()
            && let Err(e) = p.update_metadata(handle.id(), handle).await
        {
            warn!(storage=?p, error=?e, "error updating metadata")
        }
    }

    pub async fn pause(&self, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        handle.pause()?;
        handle.save_resume_stats().await;
        self.try_update_persistence_metadata(handle).await;
        Ok(())
    }

    pub async fn unpause(self: &Arc<Self>, handle: &ManagedTorrentHandle) -> anyhow::Result<()> {
        let peer_rx = self.make_peer_rx_managed_torrent(handle, true);
        handle.start(peer_rx, false)?;
        self.try_update_persistence_metadata(handle).await;
        Ok(())
    }

    pub async fn update_only_files(
        self: &Arc<Self>,
        handle: &ManagedTorrentHandle,
        only_files: &HashSet<usize>,
    ) -> anyhow::Result<()> {
        handle.update_only_files(only_files)?;
        self.try_update_persistence_metadata(handle).await;
        Ok(())
    }

    /// Bytes reserved right now by piece layer fetches (all torrents).
    pub fn fetch_budget_used(&self) -> u64 {
        self.fetch_budget.used()
    }

    /// Whether any incoming peer connection has been accepted so far.
    pub fn incoming_connection_seen(&self) -> bool {
        self.incoming_seen.load(Ordering::Relaxed)
    }

    /// The address the listeners are bound to (`None` when not listening).
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen.read().addr
    }

    /// The port announced to DHT, LSD and trackers (`None` when not
    /// listening or listening on loopback without an explicit announce port).
    pub fn announce_port(&self) -> Option<u16> {
        self.listen.read().announce_port
    }

    /// Fetches a magnet link's metadata (and a pure v2 torrent's piece
    /// layers).
    async fn resolve_magnet(
        self: &Arc<Self>,
        info_hash: Id20,
        info_hash_v2: Option<Id32>,
        peer_rx: PeerStream,
        trackers: &[(String, u32)],
        peer_opts: Option<PeerConnectionOptions>,
    ) -> anyhow::Result<ResolveMagnetResult> {
        match read_metainfo_from_peer_receiver(
            self.effective_peer_id(),
            info_hash,
            info_hash_v2,
            self.fetch_budget.clone(),
            Default::default(),
            peer_rx,
            Some(self.merge_peer_opts(peer_opts)),
            self.connector.clone(),
            self.client_name_and_version.clone(),
        )
        .await
        {
            ReadMetainfoResult::Found {
                info,
                info_bytes,
                piece_layers,
                rx,
                seen,
            } => {
                trace!(?info, "received result from DHT");
                let info = info.validate()?.with_piece_layers(piece_layers.as_deref())?;
                let metadata = TorrentMetadata::new(
                    info,
                    torrent_file_from_info_bytes(
                        info_bytes.as_ref(),
                        piece_layers.as_deref(),
                        trackers,
                    )?,
                    info_bytes.0,
                )?;
                Ok(ResolveMagnetResult {
                    metadata,
                    peer_rx: rx,
                    seen_peers: {
                        let seen = seen.into_iter().collect_vec();
                        for peer in &seen {
                            trace!(?peer, "seen")
                        }
                        seen
                    },
                })
            }
            ReadMetainfoResult::ChannelClosed { .. } => {
                bail!("input address stream exhausted, no way to discover torrent metainfo")
            }
        }
    }

    pub async fn create_and_serve_torrent(
        self: &Arc<Self>,
        path: &Path,
        opts: CreateTorrentOptions<'_>,
    ) -> Result<(CreateTorrentResult, ManagedTorrentHandle), ApiError> {
        if !path.exists() {
            return Err(ApiError::from((
                StatusCode::BAD_REQUEST,
                "path doesn't exist",
            )));
        }

        let torrent = create_torrent(path, opts, &self.spawner)
            .await
            .with_status(StatusCode::BAD_REQUEST)?;

        let bytes = torrent.as_bytes()?;

        let handle = self
            .add_torrent(
                AddTorrent::TorrentFileBytes(bytes.clone()),
                Some(AddTorrentOptions {
                    paused: false,
                    overwrite: true,
                    output_folder: Some(
                        torrent
                            .output_folder
                            .to_str()
                            .context("invalid utf-8")?
                            .to_owned(),
                    ),
                    ..Default::default()
                }),
            )
            .await?
            .into_handle()
            .context("error adding to session")?;

        Ok((torrent, handle))
    }
}

/// The web seed HTTP clients. Both use the session's proxy or bound
/// interface and follow at most 5 redirects, only to http/https.
///
/// `untrusted` is for seeds supplied by torrents that are not from a local
/// source (SSRF mitigation): host names never resolve to local addresses, and
/// redirects never lead to a local address or `localhost`, unless the seed
/// itself is on a local address and the redirect carries no query string.
pub(crate) struct WebSeedClients {
    pub trusted: reqwest::Client,
    pub untrusted: reqwest::Client,
}

impl WebSeedClients {
    /// `user_agent`: None in anonymous mode (no User-Agent header).
    pub(crate) fn build(
        proxy: Option<&crate::net::ProxyConfig>,
        bind_device_name: Option<&str>,
        user_agent: Option<&str>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            trusted: build_webseed_http_client(proxy, bind_device_name, user_agent, false)?,
            untrusted: build_webseed_http_client(proxy, bind_device_name, user_agent, true)?,
        })
    }
}

fn webseed_host_is_local(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => tracker_comms::is_local_ip(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => tracker_comms::is_local_ip(IpAddr::V6(ip)),
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost")
        }
        None => true,
    }
}

/// Whether an untrusted web seed may follow a redirect from `first` to `to`.
pub(crate) fn webseed_redirect_allowed(first: &url::Url, to: &url::Url) -> bool {
    !webseed_host_is_local(to) || (webseed_host_is_local(first) && to.query().is_none())
}

/// Resolver of the untrusted web seed client: local addresses are dropped.
struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| !tracker_comms::is_local_ip(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err("web seed host has no public address".into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

fn build_webseed_http_client(
    proxy: Option<&crate::net::ProxyConfig>,
    bind_device_name: Option<&str>,
    user_agent: Option<&str>,
    untrusted: bool,
) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .referer(false)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if !matches!(attempt.url().scheme(), "http" | "https") {
                attempt.stop()
            } else if untrusted
                && let Some(first) = attempt.previous().first()
                && !webseed_redirect_allowed(first, attempt.url())
            {
                attempt.error("redirect to a local address refused")
            } else {
                attempt.follow()
            }
        }));
    if let Some(ua) = user_agent {
        b = b.user_agent(ua);
    }
    if untrusted {
        b = b.dns_resolver(PublicOnlyResolver);
    }
    if let Some(p) = proxy {
        b = b.proxy(crate::net::reqwest_proxy(p).context("error creating proxy for web seeds")?);
    } else {
        #[cfg(not(windows))]
        if let Some(bd) = bind_device_name {
            b = b.interface(bd);
        }
        #[cfg(windows)]
        let _ = bind_device_name;
    }
    b.build().context("error building web seed HTTP client")
}

pub(crate) struct ResolveMagnetResult {
    pub metadata: TorrentMetadata,
    pub peer_rx: PeerStream,
    pub seen_peers: Vec<SocketAddr>,
}

fn remove_files_and_dirs(infos: &FileInfos, files: &dyn TorrentStorage) {
    let mut all_dirs = HashSet::new();
    for (id, fi) in infos.iter().enumerate() {
        if fi.attrs.padding {
            continue;
        }
        let mut fname = &*fi.relative_filename;
        if let Err(e) = files.remove_file(id, fname) {
            warn!(?fi.relative_filename, error=?e, "could not delete file");
        } else {
            debug!(?fi.relative_filename, "deleted the file")
        }
        while let Some(parent) = fname.parent() {
            if parent != Path::new("") {
                all_dirs.insert(parent);
            }
            fname = parent;
        }
    }

    let all_dirs = {
        let mut v = all_dirs.into_iter().collect::<Vec<_>>();
        v.sort_unstable_by_key(|p| std::cmp::Reverse(p.as_os_str().len()));
        v
    };
    for dir in all_dirs {
        if let Err(e) = files.remove_directory_if_empty(dir) {
            warn!("error removing {dir:?}: {e:#}");
        } else {
            debug!("removed {dir:?}")
        }
    }
}

#[cfg(test)]
mod tests {
    use buffers::ByteBuf;
    use itertools::Itertools;
    use librqbit_core::torrent_metainfo::{TorrentMetaV1, torrent_from_bytes};

    use super::{
        AddTorrentOptions, initial_tracker_list, is_bare_info_hash, metainfo_trackers,
        torrent_file_from_info_bytes,
    };
    use clone_to_owned::CloneToOwned;
    use std::collections::HashSet;

    const INFO: &str =
        "4:infod6:lengthi1e4:name1:a12:piece lengthi16384e6:pieces20:xxxxxxxxxxxxxxxxxxxxe";

    fn trackers_of(torrent: &str) -> Vec<(String, u32)> {
        let bytes = bytes::Bytes::from(torrent.as_bytes().to_vec());
        let parsed = torrent_from_bytes(&bytes).unwrap();
        metainfo_trackers(&parsed.clone_to_owned(Some(&bytes)))
    }

    #[test]
    fn announce_list_tiers() {
        let t = format!(
            "d8:announce12:http://x/ann13:announce-listll12:http://a/ann12:http://b/annel12:http://c/annee{INFO}e"
        );
        // announce-list wins over announce (BEP 12).
        assert_eq!(
            trackers_of(&t),
            vec![
                ("http://a/ann".into(), 0),
                ("http://b/ann".into(), 0),
                ("http://c/ann".into(), 1)
            ]
        );
        let t = format!("d8:announce12:http://x/ann{INFO}e");
        assert_eq!(trackers_of(&t), vec![("http://x/ann".into(), 0)]);
    }

    #[test]
    fn initial_list_composition() {
        let source = vec![
            ("http://a/announce".to_string(), 0),
            ("http://b/announce".to_string(), 2),
        ];
        let session: HashSet<url::Url> = ["http://s/announce".parse().unwrap()].into();
        let mut opts = AddTorrentOptions {
            trackers: Some(vec![
                "http://extra/announce".into(),
                "http://a/announce".into(),
            ]),
            tracker_entries: Some(vec![("http://e/announce".into(), 0)]),
            ..Default::default()
        };
        let list = initial_tracker_list(&source, &mut opts, false, true, &session);
        assert_eq!(
            list.entries(),
            vec![
                ("http://a/announce".into(), 0),
                ("http://e/announce".into(), 0),
                ("http://b/announce".into(), 2),
                ("http://extra/announce".into(), 3),
                ("http://s/announce".into(), 4),
            ]
        );

        // Private torrents never get the session's extra trackers.
        let mut opts = AddTorrentOptions::default();
        let list = initial_tracker_list(&source, &mut opts, true, true, &session);
        assert_eq!(list.len(), 2);

        // An explicit list replaces everything.
        let mut opts = AddTorrentOptions {
            replace_trackers: Some(vec![("udp://r:1/announce".into(), 5)]),
            trackers: Some(vec!["http://extra/announce".into()]),
            ..Default::default()
        };
        let list = initial_tracker_list(&source, &mut opts, false, true, &session);
        assert_eq!(list.entries(), vec![("udp://r:1/announce".into(), 5)]);
    }

    #[test]
    fn replaced_trackers_trust_follows_the_source() {
        let session = HashSet::new();
        let replace = vec![
            ("http://t/announce".to_string(), 0),
            ("http://user/announce".to_string(), 0),
        ];
        // A torrent from a URL, RSS or a magnet: only listed URLs are trusted.
        let mut opts = AddTorrentOptions {
            replace_trackers: Some(replace.clone()),
            trusted_urls: Some(vec!["http://user/announce".into()]),
            ..Default::default()
        };
        let list = initial_tracker_list(&[], &mut opts, false, false, &session);
        assert_eq!(list.is_trusted("http://t/announce"), Some(false));
        assert_eq!(list.is_trusted("http://user/announce"), Some(true));
        // A local .torrent: everything is trusted.
        let mut opts = AddTorrentOptions {
            replace_trackers: Some(replace),
            ..Default::default()
        };
        let list = initial_tracker_list(&[], &mut opts, false, true, &session);
        assert_eq!(list.is_trusted("http://t/announce"), Some(true));
    }

    #[test]
    fn untrusted_webseed_redirects() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        // Public seed: no redirect to local addresses or localhost.
        assert!(super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://b.example/f?x")));
        assert!(!super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://127.0.0.1/f")));
        assert!(!super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://192.168.1.1/cgi?cmd=x")));
        assert!(!super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://[::1]/f")));
        assert!(!super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://localhost./f")));
        assert!(!super::webseed_redirect_allowed(&u("http://a.example/f"), &u("http://x.localhost/f")));
        // A LAN seed may move within the LAN, but not to a URL with a query.
        assert!(super::webseed_redirect_allowed(&u("http://10.0.0.2/f"), &u("http://10.0.0.3/f")));
        assert!(!super::webseed_redirect_allowed(&u("http://10.0.0.2/f"), &u("http://10.0.0.3/f?a=b")));
    }

    #[test]
    fn test_is_bare_info_hash() {
        assert!(is_bare_info_hash(
            "08ada5a7a6183aae1e09d831df6748d566095a10"
        ));
        assert!(is_bare_info_hash(
            "08ADA5A7A6183AAE1E09D831DF6748D566095A10"
        ));
        // 40 characters, but a URL.
        assert!(!is_bare_info_hash(
            "https://nyaa.si/download/2164312.torrent"
        ));
        assert!(!is_bare_info_hash(
            "08ada5a7a6183aae1e09d831df6748d566095a1"
        ));
    }

    #[test]
    fn test_torrent_file_from_info_and_bytes() {
        fn get_trackers(info: &TorrentMetaV1<ByteBuf>) -> Vec<url::Url> {
            info.iter_announce()
                .filter_map(|t| std::str::from_utf8(t.as_ref()).ok().map(|t| t.to_owned()))
                .filter_map(|t| t.parse().ok())
                .collect_vec()
        }

        let orig_full_torrent =
            include_bytes!("../resources/ubuntu-21.04-desktop-amd64.iso.torrent");
        let parsed = torrent_from_bytes(&orig_full_torrent[..]).unwrap();
        let parsed_trackers = get_trackers(&parsed);
        let tiered = parsed_trackers
            .iter()
            .map(|u| (u.to_string(), 0))
            .collect_vec();

        let generated_torrent =
            torrent_file_from_info_bytes(parsed.info.raw_bytes.as_ref(), None, &tiered).unwrap();
        let generated_parsed = torrent_from_bytes(generated_torrent.as_ref()).unwrap();
        assert_eq!(parsed.info_hash, generated_parsed.info_hash);
        assert_eq!(parsed.info, generated_parsed.info);
        assert_eq!(parsed_trackers, get_trackers(&generated_parsed));
    }
}
