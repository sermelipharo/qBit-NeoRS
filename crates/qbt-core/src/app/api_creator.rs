//! Torrent creator. Mirrors `/api/v2/torrentcreator/*`.

use super::App;
use crate::error::QbtResult;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    pub fn creator_add_task(&self, params: TorrentCreatorParams) -> QbtResult<String> {
        self.creator.add_task(params)
    }

    pub fn creator_tasks(&self) -> Vec<CreatorTask> {
        self.creator.tasks()
    }

    pub fn creator_task(&self, id: String) -> QbtResult<CreatorTask> {
        self.creator.task(&id)
    }

    pub fn creator_torrent_file(&self, id: String) -> QbtResult<Vec<u8>> {
        self.creator.torrent_file(&id)
    }

    pub fn creator_delete_task(&self, id: String) -> QbtResult<()> {
        self.creator.delete_task(&id)
    }

    /// Piece size the creator would choose automatically for `total_size` bytes.
    pub fn creator_auto_piece_size(&self, total_size: i64, format: TorrentFormat) -> i64 {
        crate::creator::auto_piece_size(total_size, format)
    }

    /// "Calculate number of pieces" in the creator dialog (walks the source
    /// folder; `piece_size` 0 = automatic).
    pub async fn creator_calculate_pieces(
        &self,
        source_path: String,
        piece_size: i64,
        format: TorrentFormat,
    ) -> QbtResult<i64> {
        self.spawn_on_rt(async move {
            tokio::task::spawn_blocking(move || {
                crate::creator::CreatorManager::calculate_total_pieces(
                    std::path::Path::new(&source_path),
                    piece_size,
                    format,
                )
            })
            .await
            .map_err(|e| crate::error::QbtError::other(e.to_string()))?
        })
        .await
    }
}
