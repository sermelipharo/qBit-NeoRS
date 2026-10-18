//! qBit NeoRS core: a Rust reimplementation of qBittorrent's `base` and
//! `webui` layers (session management, categories/tags, queueing, share
//! limits, RSS, search, preferences, logging, WebAPI) on top of the
//! `librqbit` engine fork.

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!("qbt_core");

pub mod app;
pub mod bittorrent;
pub mod context;
pub mod creator;
pub mod error;
pub mod events;
pub mod ipfilter;
pub mod logger;
pub mod migrate;
pub mod model;
pub mod net;
pub mod prefs;
pub mod rss;
pub mod search;
pub mod tracker;
pub mod utils;
pub mod webui;

pub use app::{App, AppConfig};
pub use error::{QbtError, QbtResult};
pub use events::{CoreEvent, EventListener};
