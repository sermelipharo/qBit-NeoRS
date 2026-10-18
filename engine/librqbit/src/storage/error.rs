//! Typed storage errors, so that the application can tell "missing files" and
//! "disk full" apart from other failures of a torrent.

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::Serialize;

/// What kind of error put a torrent into the error state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TorrentErrorKind {
    /// The torrent had data, but its files are gone (moved, deleted, drive not
    /// mounted). Progress is kept; starting, rechecking or moving the torrent to
    /// where the files are recovers it.
    MissingFiles,
    /// A write failed because the disk (or the user's quota) is full.
    DiskFull,
    /// Anything else.
    Other,
}

/// A storage error with a distinct kind. It travels inside `anyhow::Error`; use
/// [`error_kind_of`] to classify an error.
#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("{} file(s) missing, e.g. {:?}", .files.len(), .files.first())]
    MissingFiles { files: Vec<PathBuf> },
    #[error("disk full writing {path:?}: {source}")]
    DiskFull {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl StorageError {
    pub fn kind(&self) -> TorrentErrorKind {
        match self {
            StorageError::MissingFiles { .. } => TorrentErrorKind::MissingFiles,
            StorageError::DiskFull { .. } => TorrentErrorKind::DiskFull,
        }
    }
}

/// Classify an error (walks the whole context chain).
pub fn error_kind_of(e: &anyhow::Error) -> TorrentErrorKind {
    if let Some(se) = e.chain().find_map(|c| c.downcast_ref::<StorageError>()) {
        return se.kind();
    }
    if e.chain()
        .filter_map(|c| c.downcast_ref::<io::Error>())
        .any(is_disk_full)
    {
        return TorrentErrorKind::DiskFull;
    }
    TorrentErrorKind::Other
}

/// Whether an I/O error means "no space left" (disk or quota).
pub fn is_disk_full(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        if let Some(code) = e.raw_os_error() {
            return code == nix::libc::ENOSPC || code == nix::libc::EDQUOT;
        }
    }
    false
}

/// Wrap an I/O error of a write operation, turning "disk full" into a typed error.
pub(crate) fn write_error(e: io::Error, path: &Path) -> anyhow::Error {
    if is_disk_full(&e) {
        return StorageError::DiskFull {
            path: path.to_owned(),
            source: e,
        }
        .into();
    }
    anyhow::Error::new(e).context(format!("error writing {path:?}"))
}

#[cfg(test)]
mod tests {
    use std::io;

    use anyhow::Context;

    use super::*;

    #[test]
    fn test_error_kind_survives_context() {
        let e: anyhow::Error = StorageError::MissingFiles {
            files: vec!["a".into()],
        }
        .into();
        let e = e.context("outer").context("outer 2");
        assert_eq!(error_kind_of(&e), TorrentErrorKind::MissingFiles);

        let e = write_error(io::Error::from(io::ErrorKind::StorageFull), Path::new("x"));
        let e = Err::<(), _>(e).context("writing chunk").unwrap_err();
        assert_eq!(error_kind_of(&e), TorrentErrorKind::DiskFull);

        #[cfg(unix)]
        {
            let e = write_error(
                io::Error::from_raw_os_error(nix::libc::ENOSPC),
                Path::new("x"),
            );
            assert_eq!(error_kind_of(&e), TorrentErrorKind::DiskFull);
        }

        let e = write_error(
            io::Error::from(io::ErrorKind::PermissionDenied),
            Path::new("x"),
        );
        assert_eq!(error_kind_of(&e), TorrentErrorKind::Other);
        assert_eq!(
            error_kind_of(&anyhow::anyhow!("whatever")),
            TorrentErrorKind::Other
        );
    }
}
