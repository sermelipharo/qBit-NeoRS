//! BitTorrent session layer: the Rust counterpart of qBittorrent's
//! `BitTorrent::SessionImpl` + `TorrentImpl`, built on the `librqbit` fork.
//!
//! Responsibilities: torrent registry and app-level torrent state (category,
//! tags, save paths, limits, share limits, queue position, added/completed
//! times, all-time counters), persistence in `BT_backup`, state mapping to
//! [`crate::model::TorrentState`], queueing, share-limit enforcement,
//! alternative-speed scheduler, watched folders, automatic torrent
//! management, excluded file names, external programs, e-mail on completion,
//! statistics, IP filter, and the data behind every properties tab.
//!
//! All engine calls go through [`engine`]; everything else is engine
//! independent.

mod actions;
mod adding;
pub mod autorun;
pub mod bencode;
pub mod categories;
pub mod engine;
pub mod metainfo;
pub mod paths;
pub mod peers;
mod policy;
pub mod queue;
pub mod resume;
pub mod scheduler;
pub mod session;
pub mod share_limits;
pub mod speed;
pub mod state;
pub mod sync;
pub mod torrent;
pub mod trackers;
pub mod trash;
mod watched;

pub use session::Session;
