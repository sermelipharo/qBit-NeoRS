//! Storage operations on a managed torrent: move storage, rename files, force
//! recheck, missing-files detection, "skip hash check" fallback, delete.
//!
//! Operations that need the torrent's I/O stopped (move, recheck) pause a live
//! torrent internally, without touching the user's paused/unpaused intent, and
//! restore it afterwards. While such an operation runs, start/pause requests only
//! record the intent, which the operation then honors.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, bail};
use librqbit_core::lengths::ValidPieceIndex;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::storage::{
    MoveMode, MoveProgress, StorageError, StorageLayout, TorrentErrorKind, error::error_kind_of,
    layout::TrashRef,
};

use super::{
    ManagedTorrent, ManagedTorrentState, initializing::TorrentStateInitializing,
    paused::TorrentStatePaused,
};

/// What [`ManagedTorrent::quiesce`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quiesced {
    WasLive,
    WasInitializing,
    Idle,
}

impl ManagedTorrent {
    /// The torrent's on-disk layout (paths, decorations, move progress).
    pub fn storage_layout(&self) -> &Arc<StorageLayout> {
        &self.shared.layout
    }

    /// User-visible relative paths of the files (renames applied, without the
    /// incomplete suffix or `.unwanted` folder).
    pub fn file_paths(&self) -> Vec<PathBuf> {
        self.shared.layout.user_paths()
    }

    /// Relative on-disk paths of the files (with suffix / `.unwanted` decorations).
    pub fn actual_file_paths(&self) -> Vec<PathBuf> {
        self.shared.layout.actual_paths()
    }

    /// Files whose path differs from the torrent's own (for persistence; pass back
    /// as [`AddTorrentOptions::renamed_files`](crate::AddTorrentOptions::renamed_files)).
    pub fn renamed_files(&self) -> BTreeMap<usize, PathBuf> {
        match &*self.metadata.load() {
            Some(m) => self.shared.layout.renamed_files(&m.file_infos),
            None => BTreeMap::new(),
        }
    }

    /// Progress of a running [`move_storage`](Self::move_storage).
    pub fn move_progress(&self) -> Option<MoveProgress> {
        self.shared.layout.move_progress()
    }

    /// The kind of error the torrent is in, if it's in the error state.
    pub fn error_kind(&self) -> Option<TorrentErrorKind> {
        self.with_state(|s| match s {
            ManagedTorrentState::Error(e) => Some(error_kind_of(e)),
            _ => None,
        })
    }

    async fn persist_storage_changes(&self) {
        if let Some(session) = self.shared.session.upgrade()
            && let Some(me) = session.get(self.shared.id.into())
        {
            session.try_update_persistence_metadata(&me).await;
        }
    }

    /// Rename one file. See [`rename_files`](Self::rename_files).
    pub async fn rename_file(
        self: &Arc<Self>,
        index: usize,
        new_relative_path: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        self.rename_files(vec![(index, new_relative_path.as_ref().to_owned())])
            .await
    }

    /// Rename files (and, by renaming every file under it, folders) inside the
    /// torrent. Paths are relative to the output folder; they are validated as a
    /// batch (no `..`, absolute paths, empty components, NUL, or clashes with other
    /// files). Data already on disk is renamed immediately, also while the torrent
    /// is live; all later I/O uses the new paths. Persisted with the session.
    pub async fn rename_files(
        self: &Arc<Self>,
        renames: Vec<(usize, PathBuf)>,
    ) -> anyhow::Result<()> {
        let layout = self.shared.layout.clone();
        self.shared
            .spawner
            .block_in_place_with_semaphore(|| layout.rename_files(&renames))
            .await?;
        self.persist_storage_changes().await;
        Ok(())
    }

    /// Stop all I/O of the torrent: pause it if live, stop a running check.
    async fn quiesce(self: &Arc<Self>) -> anyhow::Result<Quiesced> {
        loop {
            {
                let mut g = self.locked.write();
                match &g.state {
                    ManagedTorrentState::Live(live) => {
                        let paused = live.pause()?;
                        g.state = ManagedTorrentState::Paused(paused);
                        self.state_change_notify.notify_waiters();
                        return Ok(Quiesced::WasLive);
                    }
                    ManagedTorrentState::Initializing(init) => {
                        init.request_pause();
                        if !init.is_check_running() {
                            return Ok(Quiesced::WasInitializing);
                        }
                    }
                    ManagedTorrentState::Paused(_) | ManagedTorrentState::Error(_) => {
                        return Ok(Quiesced::Idle);
                    }
                    ManagedTorrentState::None => bail!("bug: torrent is in empty state"),
                }
            }
            let _ = timeout(
                Duration::from_millis(50),
                self.state_change_notify.notified(),
            )
            .await;
        }
    }

    fn start_with_intent(self: &Arc<Self>) -> anyhow::Result<()> {
        let paused = self.locked.read().paused;
        let peer_rx = if paused {
            None
        } else {
            let session = self.shared.session.upgrade().context("session is dead")?;
            session.make_peer_rx_managed_torrent(self, true)
        };
        self.start(peer_rx, paused)
    }

    /// Bring the torrent back to what the user wants after a storage operation.
    fn restore_after(self: &Arc<Self>, q: Quiesced) -> anyhow::Result<()> {
        let (paused_intent, initializing, paused) = {
            let g = self.locked.read();
            (
                g.paused,
                matches!(g.state, ManagedTorrentState::Initializing(_)),
                matches!(g.state, ManagedTorrentState::Paused(_)),
            )
        };
        if (q == Quiesced::WasInitializing && initializing) || (paused && !paused_intent) {
            self.start_with_intent()
        } else {
            Ok(())
        }
    }

    /// Replace the current (quiesced) state with a fresh check and start it.
    /// `full` throws away fastresume data and hashes everything.
    fn reinitialize(self: &Arc<Self>, full: bool) -> anyhow::Result<()> {
        let metadata = self
            .metadata
            .load_full()
            .context("torrent is not resolved")?;
        {
            let mut g = self.locked.write();
            let files = match g.state.take() {
                ManagedTorrentState::Paused(p) => p.files,
                ManagedTorrentState::Initializing(init) => init.files.take()?,
                ManagedTorrentState::Error(e) => {
                    match self
                        .shared
                        .storage_factory
                        .create_and_init(self.shared(), &metadata)
                    {
                        Ok(files) => files,
                        Err(err) => {
                            g.state = ManagedTorrentState::Error(e);
                            return Err(err);
                        }
                    }
                }
                ManagedTorrentState::Live(live) => {
                    g.state = ManagedTorrentState::Live(live);
                    bail!("bug: torrent is live while it should be quiesced");
                }
                ManagedTorrentState::None => bail!("bug: torrent is in empty state"),
            };
            g.state = ManagedTorrentState::Initializing(Arc::new(TorrentStateInitializing::new(
                self.shared.clone(),
                metadata,
                g.only_files.clone(),
                files,
                full,
            )));
            self.state_change_notify.notify_waiters();
        }
        self.start_with_intent()
    }

    /// Move the torrent's files to `new_output_folder` (absolute) and use it from
    /// now on (persisted).
    ///
    /// Works for live and paused torrents: a live torrent is paused internally for
    /// the duration (peers are disconnected), files are renamed on the same volume
    /// or copied, fsynced and deleted across volumes (progress in
    /// [`move_progress`](Self::move_progress)), then the torrent resumes at the new
    /// location. Files at the destination are handled according to `mode`;
    /// [`MoveMode::DontReplace`] rechecks the torrent if it kept any. On failure the
    /// moved files are moved back and the torrent stays where it was. A torrent with
    /// missing files is re-checked at the new location, so moving it to where its
    /// data is recovers it.
    pub async fn move_storage(
        self: &Arc<Self>,
        new_output_folder: PathBuf,
        mode: MoveMode,
    ) -> anyhow::Result<()> {
        if !new_output_folder.is_absolute() {
            bail!("the new output folder must be an absolute path: {new_output_folder:?}");
        }
        let layout = self.shared.layout.clone();
        let _op = layout.op_lock().lock().await;
        if layout.output_folder() == new_output_folder {
            return Ok(());
        }

        layout.set_busy(true);
        let q = match self.quiesce().await {
            Ok(q) => q,
            Err(e) => {
                layout.set_busy(false);
                return Err(e);
            }
        };
        let was_missing_files = self.error_kind() == Some(TorrentErrorKind::MissingFiles);
        let from = layout.output_folder();
        info!(
            id = self.shared.id,
            ?from,
            to = ?new_output_folder,
            ?mode,
            "moving torrent storage"
        );

        let state = layout.begin_move(new_output_folder.clone());
        let result = {
            let layout = layout.clone();
            let dst = new_output_folder.clone();
            let state = state.clone();
            tokio::task::spawn_blocking(move || layout.move_to(&dst, mode, &state))
                .await
                .context("move task panicked")
                .and_then(|r| r)
        };
        layout.end_move();
        layout.set_busy(false);

        match result {
            Ok(outcome) => {
                if outcome.moved {
                    info!(id = self.shared.id, to = ?new_output_folder, "moved torrent storage");
                    self.persist_storage_changes().await;
                }
                if outcome.need_recheck {
                    self.reinitialize(true)
                } else if was_missing_files {
                    // Fastresume data is kept: if the files are here, it's validated.
                    self.reinitialize(false)
                } else {
                    self.restore_after(q)
                }
            }
            Err(e) => {
                warn!(id = self.shared.id, ?from, to = ?new_output_folder, "failed to move torrent: {e:#}");
                if let Err(e) = self.restore_after(q) {
                    warn!(
                        id = self.shared.id,
                        "error restarting torrent after failed move: {e:#}"
                    );
                }
                Err(e)
            }
        }
    }

    /// Re-hash all data and rebuild the have-bitfield (fastresume data is thrown
    /// away). Works for live, paused, checking and errored torrents. The torrent
    /// goes through the initializing state (progress in the stats'
    /// `progress_bytes`), respecting the session's limit on concurrent checks, and
    /// then returns to running or paused as the user had it. Missing pieces get
    /// downloaded again.
    pub async fn force_recheck(self: &Arc<Self>) -> anyhow::Result<()> {
        if self.metadata.load().is_none() {
            bail!("torrent is not resolved");
        }
        let layout = self.shared.layout.clone();
        let _op = layout.op_lock().lock().await;
        layout.set_busy(true);
        let r = self.quiesce().await;
        layout.set_busy(false);
        r?;
        layout.set_unverified(None);
        info!(id = self.shared.id, "force recheck");
        self.reinitialize(true)
    }

    /// Called (from the read path) when a piece assumed present because of "skip
    /// hash check" fails verification: mark it missing so it gets downloaded again.
    pub(crate) fn on_piece_verification_failed(&self, piece: ValidPieceIndex) {
        warn!(
            id = self.shared.id,
            piece = piece.get(),
            "piece assumed present (skip hash check) failed verification, marking it missing"
        );
        let Some(metadata) = self.metadata.load_full() else {
            return;
        };
        let live = {
            let mut g = self.locked.write();
            match &mut g.state {
                ManagedTorrentState::Paused(p) => {
                    p.chunk_tracker
                        .mark_piece_missing(piece, &metadata.file_infos);
                    None
                }
                ManagedTorrentState::Live(l) => Some(l.clone()),
                _ => None,
            }
        };
        if let Some(live) = live {
            {
                let mut g = live.lock_write("piece_verification_failed");
                if let Ok(pieces) = g.get_pieces_mut()
                    && pieces.mark_piece_missing(piece, &metadata.file_infos)
                    && let Err(e) = pieces.flush_have_pieces(true)
                {
                    warn!(id = self.shared.id, "error flushing bitfield: {e:#}");
                }
            }
            // We may have dropped peers because we were finished.
            live.reconnect_all_not_needed_peers();
        }
        self.shared.layout.on_piece_missing(piece);
    }

    /// The wanted set of files changed: move files in and out of `.unwanted`.
    pub(crate) fn on_files_wanted_changed(&self, wanted: impl Fn(usize) -> bool) {
        let layout = &self.shared.layout;
        layout.set_wanted(wanted);
        if layout.settings().use_unwanted_folder {
            self.shared
                .spawner
                .block_in_place(|| layout.reconcile_all());
        }
    }

    /// When going live: the torrent had data in files that no longer exist.
    pub(crate) fn missing_files_error(&self, paused: &TorrentStatePaused) -> Option<anyhow::Error> {
        let per_file = paused.chunk_tracker.per_file_have_bytes();
        let missing = self
            .shared
            .layout
            .missing_files(|i| per_file.get(i).is_some_and(|b| *b > 0));
        if missing.is_empty() {
            return None;
        }
        warn!(
            id = self.shared.id,
            count = missing.len(),
            first = ?missing.first(),
            "files with downloaded data are missing"
        );
        Some(StorageError::MissingFiles { files: missing }.into())
    }

    /// Delete the torrent's files (see [`StorageLayout`]'s rules: only its own
    /// files, empty folders it leaves behind and an owned output folder).
    pub(crate) fn delete_files_from_disk(&self, trash: Option<TrashRef<'_>>) -> Vec<anyhow::Error> {
        let errors = self.shared.layout.delete_files(trash);
        for e in &errors {
            warn!(id = self.shared.id, "{e:#}");
        }
        debug!(id = self.shared.id, "deleted files");
        errors
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use buffers::ByteBufOwned;
    use librqbit_core::torrent_metainfo::{TorrentMetaV1File, TorrentMetaV1Info};

    use crate::{storage::paths::validate_relative_path, torrent_state::TorrentMetadata};

    fn file(path: &[&[u8]]) -> TorrentMetaV1File<ByteBufOwned> {
        TorrentMetaV1File {
            length: 1,
            path: path.iter().map(|p| ByteBufOwned::from(*p)).collect(),
            attr: None,
            sha1: None,
            symlink_path: None,
        }
    }

    #[test]
    fn test_hostile_torrent_names_are_sanitized() {
        let info = TorrentMetaV1Info::<ByteBufOwned> {
            name: Some(ByteBufOwned::from(&b"x"[..])),
            pieces: ByteBufOwned::from(vec![0u8; 20]),
            piece_length: 16384,
            files: Some(vec![
                file(&[b".", b"a"]),
                file(&[b"", b"b"]),
                file(&[b"c\0d"]),
                file(&[b"_", b"a"]),
                file(&[b"ok"]),
            ]),
            ..Default::default()
        };
        // Upstream validation lets these through (it only rejects ".." and separators).
        let validated = info.validate().unwrap();
        let m = TorrentMetadata::new(validated, Default::default(), Default::default()).unwrap();
        let mut seen = HashSet::new();
        for fi in &m.file_infos {
            validate_relative_path(&fi.relative_filename).unwrap();
            assert!(seen.insert(fi.relative_filename.clone()), "{fi:?}");
        }
        assert_eq!(m.file_infos[4].relative_filename.to_str(), Some("ok"));
    }
}
