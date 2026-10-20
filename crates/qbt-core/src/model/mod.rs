//! Data transfer objects shared by every front end: the SwiftUI app (through
//! UniFFI), the WebAPI server (through serde, using the exact qBittorrent
//! WebAPI v2 JSON names) and the headless daemon.
//!
//! Naming: Rust fields are idiomatic, `#[serde(rename)]` restores the WebAPI
//! key where it differs. Values follow qBittorrent semantics (e.g. `-1` for
//! "unknown/unlimited", `8640000` for an infinite ETA) so the WebAPI layer can
//! serialize them unchanged.

mod creator;
mod log;
mod names;
mod rss;
mod search;
mod torrent;
mod transfer;

pub use creator::*;
pub use log::*;
pub use names::*;
pub use rss::*;
pub use search::*;
pub use torrent::*;
pub use transfer::*;

/// ETA value qBittorrent uses for "infinity" (100 days).
pub const MAX_ETA: i64 = 8_640_000;
/// Ratios at or above this are reported as -1 / "∞".
pub const MAX_RATIO: f64 = 9999.0;
/// Upstream qBittorrent version this build is feature-compatible with.
pub const QBT_COMPAT_VERSION: &str = "5.2.4";
/// WebAPI version string reported by `/api/v2/app/webapiVersion`.
pub const WEBAPI_VERSION: &str = "2.15.1";
