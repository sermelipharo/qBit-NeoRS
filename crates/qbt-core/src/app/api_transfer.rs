//! Global transfer state: status bar, speed limits, statistics, speed graph.
//! Mirrors `/api/v2/transfer/*`.

use super::App;
use crate::error::QbtResult;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    pub fn server_state(&self) -> ServerState {
        self.session.server_state()
    }

    /// Whether the whole BitTorrent session is paused (runtime state; torrents
    /// keep their own started/stopped state). Starts true with `advanced.start_paused`.
    pub fn is_session_paused(&self) -> bool {
        self.session.is_paused()
    }

    /// Pauses all transfers without changing any torrent's state.
    pub async fn pause_session(&self) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move {
            s.pause_session().await;
            Ok(())
        }).await
    }

    pub async fn resume_session(&self) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move {
            s.resume_session().await;
            Ok(())
        }).await
    }

    pub fn alt_speed_limits_enabled(&self) -> bool {
        self.session.alt_speed_limits_enabled()
    }

    pub fn set_alt_speed_limits_enabled(&self, enabled: bool) {
        self.session.set_alt_speed_limits_enabled(enabled)
    }

    /// Currently effective global download limit (normal or alternative), bytes/s, 0 = unlimited.
    pub fn global_download_limit(&self) -> i64 {
        self.session.global_download_limit()
    }

    /// Sets the limit of the currently active mode (normal or alternative).
    pub fn set_global_download_limit(&self, limit: i64) -> QbtResult<()> {
        self.session.set_global_download_limit(limit)
    }

    pub fn global_upload_limit(&self) -> i64 {
        self.session.global_upload_limit()
    }

    pub fn set_global_upload_limit(&self, limit: i64) -> QbtResult<()> {
        self.session.set_global_upload_limit(limit)
    }

    /// Up to the last `seconds` of 1 s samples for the Speed graph tab.
    pub fn speed_history(&self, seconds: u32) -> Vec<SpeedSample> {
        self.session.speed_history(seconds)
    }

    /// Speed graph points of the last `seconds`, averaged into clock-aligned
    /// buckets of `resolution` seconds (up to 24 h of history), only those
    /// newer than `after_ms` (Unix ms; 0 = all). Buckets still filling are
    /// left out, so a caller can append what it gets to what it has.
    pub fn speed_history_points(&self, seconds: u32, resolution: u32, after_ms: i64) -> Vec<SpeedSample> {
        self.session.speed_history_points(seconds, resolution, after_ms)
    }
}
