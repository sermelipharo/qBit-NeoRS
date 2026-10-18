//! BitTorrent tracker protocol: HTTP(S) and UDP announces and scrapes, a
//! hardened response parser, the network policy applied to tracker requests
//! (SSRF mitigation, IP filter, redirects) and the tiered tracker list state
//! machine that decides when to announce to which tracker.

pub mod bdecode;
mod client;
mod error;
mod policy;
mod response;
mod tiers;
mod tracker_comms_http;
mod tracker_comms_udp;

pub use client::{
    AddressFamily, AnnounceOutcome, CallOptions, EndpointOutcome, HttpClientFactory,
    HttpClientOptions, I2pPeerMapper, I2pTransport, TrackerClient,
};
pub use error::{TrackerError, TrackerErrorKind};
pub use policy::{
    IpFilterFn, MAX_REDIRECTS, MAX_URL_LEN, NetPolicy, Purpose, is_local_ip, is_supported_scheme,
    parse_tracker_url, redact_url,
};
pub use response::{
    AnnounceResponse, MAX_MESSAGE_LEN, MAX_PEERS_PER_RESPONSE, ScrapeData, parse_announce_response,
    parse_i2p_peers, parse_scrape_response, sanitize_message,
};
pub use tiers::{
    AnnounceSettings, AppliedAnnounce, MAX_TRACKERS, PlannedAnnounce, PlannedScrape,
    SCRAPE_INTERVAL, TrackerEditError, TrackerEndpointStatus, TrackerId, TrackerList,
    TrackerOrigin, TrackerState, TrackerStatus, retry_delay,
};
pub use tracker_comms_http::{
    AnnounceEvent, AnnounceRequest, MAX_RESPONSE_SIZE, scrape_url, url_with_params,
};
pub use tracker_comms_udp::UdpTrackerClient;
