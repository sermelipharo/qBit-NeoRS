//! Execution log. Mirrors `/api/v2/log/*`.

use super::App;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// Messages with id > `last_known_id` whose level is in `levels`
    /// (empty = all levels).
    pub fn main_log(&self, last_known_id: i64, levels: Vec<LogLevel>) -> Vec<LogMessage> {
        self.ctx.logger.messages(last_known_id, &levels)
    }

    pub fn peer_log(&self, last_known_id: i64) -> Vec<PeerLogEntry> {
        self.ctx.logger.peers(last_known_id)
    }

    pub fn clear_log(&self) {
        self.ctx.logger.clear()
    }
}
