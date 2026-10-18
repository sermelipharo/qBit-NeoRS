//!
//! This crate provides everything necessary to download [torrents](https://en.wikipedia.org/wiki/BitTorrent).
//!
//! # Quick usage example
//!
//! ```no_run
//! use librqbit::*;
//!
//! tokio_test::block_on(async {
//!     let session = Session::new("/tmp/where-to-download".into()).await.unwrap();
//!     let managed_torrent_handle = session.add_torrent(
//!        AddTorrent::from_url("magnet:?xt=urn:btih:cab507494d02ebb1178b38f2e9d7be299c86b862"),
//!        None // options
//!     ).await.unwrap().into_handle().unwrap();
//!     managed_torrent_handle.wait_until_completed().await.unwrap();
//! })
//! ```
//!
//! # Overview
//! The main type to start off with is [`Session`].
//!
//! It also proved useful to use the [`Api`] when building the rqbit desktop app, as it provides
//! a facade that works with simple serializable types.
//!

#![warn(clippy::cast_possible_truncation)]

macro_rules! aframe {
    ($e:expr) => {{
        #[cfg(feature = "async-bt")]
        {
            async_backtrace::frame!($e)
        }
        #[cfg(not(feature = "async-bt"))]
        {
            $e
        }
    }};
}

#[macro_use]
mod stat_gen;

pub mod api;
mod api_error;
mod bitv;
mod bitv_factory;
mod chunk_tracker;
mod conn_limiter;
mod create_torrent_file;
mod dht_utils;
mod error;
pub mod file_info;
mod file_ops;
mod i2p;
mod i2p_http;
#[cfg(feature = "http-api")]
pub mod http_api;
#[cfg(feature = "http-api-client")]
pub mod http_api_client;
#[cfg(any(feature = "http-api", feature = "http-api-client"))]
pub mod http_api_types;
mod ip_ranges;
pub mod limits;
mod listen;
pub mod net;
mod merge_streams;
mod mixed_mode;
mod peer_client;
mod peer_connection;
mod peer_info_reader;
mod peer_policy;
pub mod peer_source;
mod piece_tracker;
mod read_buf;
mod session;
mod session_persistence;
pub mod session_stats;
mod session_trackers;
pub mod spawn_utils;

pub mod storage;
mod stream_connect;
mod torrent_state;
#[cfg(feature = "tracing-subscriber-utils")]
pub mod tracing_subscriber_config_utils;
mod type_aliases;
mod vectored_traits;
#[cfg(feature = "watch")]
pub mod watch;

pub use error::{Error, Result};

pub use api::Api;
pub use api_error::{ApiError, WithStatus, WithStatusError};
pub use create_torrent_file::{CreateTorrentOptions, CreateTorrentResult, create_torrent};
pub use dht;
pub use librqbit_core::spawn_utils::spawn as librqbit_spawn;
pub use listen::{ListenerMode, ListenerOptions};
pub use peer_client::client_name_from_peer_id;
pub use peer_connection::PeerConnectionOptions;
pub use mixed_mode::MixedModeAlgorithm;
pub use peer_policy::{PeerPolicyOptions, SessionTransferStats};
pub use peer_source::{DisabledDiscovery, PeerSource, PeerSourceCounts, PeerSources};
pub use session::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DeleteOptions, DhtSessionConfig,
    ListOnlyResponse, SUPPORTED_SCHEMES, Session, SessionOptions, SessionPersistenceConfig,
};
pub use i2p::{I2pSettings, I2pStatus, SamOptions};
pub use net::{
    BlockedPeer, BlockedPeerCallback, EncryptionPolicy, MappingProtocol, MappingTransport,
    NetSettings, PeerBlockReason, PortMapping, PortMappingConfig, PortMappingStatus, ProxyConfig,
    ProxyKind, ProxySettings,
};
pub use stream_connect::{ConnectionKind, ConnectionOptions};
pub use torrent_state::live::{
    choker::{ChokingAlgorithm, SeedChokingAlgorithm},
    peer_details::{PeerDetails, PeerFlags},
    priorities::{
        PIECE_STATE_DOWNLOADING, PIECE_STATE_HAVE, PIECE_STATE_MISSING, PRIORITY_DEFAULT,
        PRIORITY_IGNORE, PRIORITY_MAX,
    },
};
pub use session_trackers::TrackerSettings;
pub use torrent_state::trackers::TorrentTrackers;
pub use torrent_state::webseeds::{WebSeedEditError, WebSeedState, WebSeedStatus};
pub use torrent_state::{
    ManagedTorrent, ManagedTorrentShared, ManagedTorrentState, TorrentMetadata, TorrentStats,
    TorrentStatsState,
};
pub use tracker_comms::{
    AddressFamily, IpFilterFn, TrackerEditError, TrackerEndpointStatus, TrackerState, TrackerStatus,
};
pub use type_aliases::FileInfos;

pub use buffers::*;
pub use clone_to_owned::CloneToOwned;
pub use librqbit_core::magnet::*;
pub use librqbit_core::peer_id::*;
pub use librqbit_core::torrent_metainfo::*;
pub use librqbit_core::v2;

#[cfg(test)]
mod tests;

/// The cargo version of librqbit.
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub const fn client_name_and_version() -> &'static str {
    concat!("rqbit ", env!("CARGO_PKG_VERSION"))
}

pub fn try_increase_nofile_limit() -> anyhow::Result<u64> {
    Ok(rlimit::increase_nofile_limit(1024 * 1024)?)
}
