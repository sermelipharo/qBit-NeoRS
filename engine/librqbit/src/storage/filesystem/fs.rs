use std::{
    fs::File,
    io::IoSlice,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::bail;
use librqbit_core::lengths::ValidPieceIndex;
use tracing::warn;

use crate::{
    storage::{StorageFactoryExt, layout::StorageLayout},
    torrent_state::{ManagedTorrentShared, TorrentMetadata},
};

use crate::storage::{StorageFactory, TorrentStorage};

#[derive(Default, Clone, Copy)]
pub struct FilesystemStorageFactory {}

impl StorageFactory for FilesystemStorageFactory {
    type Storage = FilesystemStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<FilesystemStorage> {
        Ok(FilesystemStorage {
            layout: shared.layout.clone(),
            allow_overwrite: shared.options.allow_overwrite,
            alive: AtomicBool::new(true),
        })
    }

    fn clone_box(&self) -> crate::storage::BoxStorageFactory {
        self.boxed()
    }
}

/// Files on a local file system.
///
/// Paths come from the torrent's [`StorageLayout`] (output folder, renames,
/// decorations); file handles are opened lazily and cached in the session-wide
/// file pool.
pub struct FilesystemStorage {
    layout: Arc<StorageLayout>,
    allow_overwrite: bool,
    // Cleared by take(): the instance is then unusable, like upstream's.
    alive: AtomicBool,
}

impl FilesystemStorage {
    fn check_alive(&self) -> crate::Result<()> {
        if self.alive.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(crate::Error::FsFileIsNone)
        }
    }

    pub(crate) fn take_fs(&self) -> anyhow::Result<Self> {
        self.alive.store(false, Ordering::Release);
        Ok(Self {
            layout: self.layout.clone(),
            allow_overwrite: self.allow_overwrite,
            alive: AtomicBool::new(true),
        })
    }

    /// Open a file read-write outside of the handle pool, e.g. to memory-map it.
    pub fn open_file_rw(&self, file_id: usize) -> anyhow::Result<File> {
        self.check_alive()?;
        self.layout.open_rw_unpooled(file_id)
    }
}

impl TorrentStorage for FilesystemStorage {
    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.check_alive()?;
        self.layout.read_at(file_id, offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        self.check_alive()?;
        self.layout.write_at(file_id, offset, buf)
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        self.check_alive()?;
        self.layout.write_vectored_at(file_id, offset, bufs)
    }

    fn remove_file(&self, file_id: usize, _filename: &Path) -> anyhow::Result<()> {
        // The layout knows where the file actually is (renames, decorations).
        let path = self
            .layout
            .full_actual_path(file_id)
            .ok_or(crate::Error::FsFileIsNone)?;
        self.layout.close_handles();
        Ok(std::fs::remove_file(path)?)
    }

    fn ensure_file_length(&self, file_id: usize, len: u64) -> anyhow::Result<()> {
        self.check_alive()?;
        self.layout.ensure_len(file_id, len)
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(self.take_fs()?))
    }

    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        let path = self.layout.output_folder().join(path);
        if !path.is_dir() {
            bail!("cannot remove dir: {path:?} is not a directory")
        }
        if std::fs::read_dir(&path)?.next().is_none() {
            std::fs::remove_dir(&path)?;
        } else {
            warn!("did not remove {path:?} as it was not empty");
        }
        Ok(())
    }

    fn init(
        &mut self,
        _shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        // Files are opened lazily. Find existing data (also under decorated names).
        self.layout.resolve_existing_paths();
        if !self.allow_overwrite
            && let Some(existing) = self.layout.existing_files().into_iter().next()
        {
            bail!(
                "error creating a new file (because allow_overwrite = false) {:?}: file exists",
                existing
            );
        }
        Ok(())
    }

    fn on_piece_completed(&self, piece_index: ValidPieceIndex) -> anyhow::Result<()> {
        self.layout.on_piece_completed(piece_index);
        Ok(())
    }

    fn layout(&self) -> Option<&StorageLayout> {
        Some(&self.layout)
    }
}
