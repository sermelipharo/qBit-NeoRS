//! Thin wrappers over platform file APIs used by the filesystem storage.

use std::{
    fs::{File, OpenOptions},
    io::{self, IoSlice, Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::storage::settings::DiskCacheMode;

/// Ask the OS not to cache this file's data (macOS `F_NOCACHE`). Best effort.
#[cfg(target_vendor = "apple")]
fn disable_os_cache(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl on a valid, owned file descriptor with an integer argument.
    let r = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_NOCACHE, 1) };
    if r == -1 {
        tracing::debug!("F_NOCACHE failed: {:#}", io::Error::last_os_error());
    }
}

#[cfg(not(target_vendor = "apple"))]
fn disable_os_cache(_file: &File) {}

/// Open an existing file for reading.
pub(crate) fn open_read(path: &Path, cache: DiskCacheMode) -> io::Result<File> {
    let f = OpenOptions::new().read(true).open(path)?;
    if cache == DiskCacheMode::Disabled {
        disable_os_cache(&f);
    }
    Ok(f)
}

/// Open (creating if needed, along with missing parent directories) a file for
/// reading and writing. Never truncates.
pub(crate) fn open_read_write(path: &Path, cache: DiskCacheMode) -> io::Result<File> {
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
    };
    let f = match open() {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            open()?
        }
        Err(e) => return Err(e),
    };
    if cache == DiskCacheMode::Disabled {
        disable_os_cache(&f);
    }
    #[cfg(windows)]
    {
        let marked = super::sparse::mark_file_sparse(&f);
        tracing::trace!(?path, marked, "marking sparse");
    }
    Ok(f)
}

pub(crate) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut offset, mut buf) = (offset, buf);
        while !buf.is_empty() {
            let n = file.seek_read(buf, offset)?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            offset += n as u64;
            buf = &mut buf[n..];
        }
        Ok(())
    }
}

pub(crate) fn write_all_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.write_all_at(buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let (mut offset, mut buf) = (offset, buf);
        while !buf.is_empty() {
            let n = file.seek_write(buf, offset)?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            offset += n as u64;
            buf = &buf[n..];
        }
        Ok(())
    }
}

/// Write both buffers at `offset`, with one `pwritev` call where possible. Handles
/// short writes. Returns the number of bytes written (always the sum of lengths).
pub(crate) fn write_vectored_at(
    file: &File,
    offset: u64,
    bufs: [IoSlice<'_>; 2],
) -> io::Result<usize> {
    let (l0, l1) = (bufs[0].len(), bufs[1].len());
    let total = l0 + l1;
    #[cfg(unix)]
    let written = match nix::libc::off_t::try_from(offset) {
        Ok(off) => nix::sys::uio::pwritev(file, &bufs, off).map_err(io::Error::from)?,
        Err(_) => 0,
    };
    #[cfg(not(unix))]
    let written = 0usize;

    if written < l0 {
        write_all_at(file, offset + written as u64, &bufs[0][written..])?;
        write_all_at(file, offset + l0 as u64, &bufs[1])?;
    } else if written < total {
        let in_second = written - l0;
        write_all_at(file, offset + written as u64, &bufs[1][in_second..])?;
    }
    Ok(total)
}

/// Grow `file` to `len` bytes, reserving the disk blocks up front. Falls back to a
/// plain (sparse) `set_len` when the platform or file system can't preallocate.
/// Never shrinks and never touches existing data.
pub(crate) fn preallocate(file: &File, len: u64) -> io::Result<()> {
    let current = file.metadata()?.len();
    if current >= len {
        return Ok(());
    }
    if let Err(e) = preallocate_native(file, current, len) {
        tracing::debug!("preallocation not available, using a sparse file: {e:#}");
    }
    if file.metadata()?.len() < len {
        file.set_len(len)?;
    }
    Ok(())
}

#[cfg(target_vendor = "apple")]
fn preallocate_native(file: &File, current: u64, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let length = nix::libc::off_t::try_from(len - current)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut store = nix::libc::fstore_t {
        fst_flags: nix::libc::F_ALLOCATECONTIG | nix::libc::F_ALLOCATEALL,
        fst_posmode: nix::libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: length,
        fst_bytesalloc: 0,
    };
    // SAFETY: valid fd, and `store` is a properly initialized fstore_t that outlives the call.
    let mut r = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_PREALLOCATE, &mut store) };
    if r == -1 {
        // Contiguous space may not be available; any space will do.
        store.fst_flags = nix::libc::F_ALLOCATEALL;
        // SAFETY: as above.
        r = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_PREALLOCATE, &mut store) };
    }
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    // F_PREALLOCATE reserves blocks but doesn't change the size.
    file.set_len(len)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn preallocate_native(file: &File, current: u64, len: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let off = nix::libc::off_t::try_from(current)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let size = nix::libc::off_t::try_from(len - current)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: valid fd; mode 0 allocates and extends the file size.
    let r = unsafe { nix::libc::fallocate(file.as_raw_fd(), 0, off, size) };
    if r == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn preallocate_native(_file: &File, _current: u64, _len: u64) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Whether a rename failed because source and destination are on different volumes.
pub(crate) fn is_cross_device(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::CrossesDevices {
        return true;
    }
    #[cfg(unix)]
    if e.raw_os_error() == Some(nix::libc::EXDEV) {
        return true;
    }
    false
}

/// Copy `src` to `dst` (replacing it), report progress, and fsync the result.
pub(crate) fn copy_file_with_progress(
    src: &Path,
    dst: &Path,
    progress: &AtomicU64,
) -> io::Result<()> {
    let mut input = File::open(src)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dst)?;
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        output.write_all(&buf[..n])?;
        progress.fetch_add(n as u64, Ordering::Relaxed);
    }
    if let Ok(meta) = input.metadata() {
        let _ = output.set_permissions(meta.permissions());
        if let Ok(mtime) = meta.modified() {
            let _ = output.set_modified(mtime);
        }
    }
    output.sync_all()
}

/// Whether two paths point to the same file system object.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(ca), Ok(cb)) => ca == cb,
            _ => false,
        }
    }
}

/// Free space available to the user on the volume holding `path`, in bytes.
pub fn available_space(path: &Path) -> io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        #[cfg(target_vendor = "apple")]
        {
            // statvfs has 32-bit block counts on macOS; statfs doesn't.
            // SAFETY: statfs is plain old data, filled in by the call below.
            let mut st: nix::libc::statfs = unsafe { std::mem::zeroed() };
            // SAFETY: valid NUL-terminated path and a valid out pointer.
            if unsafe { nix::libc::statfs(c.as_ptr(), &mut st) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(st.f_bavail.saturating_mul(u64::from(st.f_bsize)))
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            // SAFETY: statvfs is plain old data, filled in by the call below.
            let mut st: nix::libc::statvfs = unsafe { std::mem::zeroed() };
            // SAFETY: valid NUL-terminated path and a valid out pointer.
            if unsafe { nix::libc::statvfs(c.as_ptr(), &mut st) } != 0 {
                return Err(io::Error::last_os_error());
            }
            #[allow(clippy::useless_conversion)]
            Ok(u64::from(st.f_bavail).saturating_mul(u64::from(st.f_frsize)))
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(test)]
mod tests {
    use std::{io::IoSlice, sync::atomic::AtomicU64};

    use super::*;

    #[test]
    fn test_preallocate_and_vectored_write() {
        let dir = tempfile::TempDir::with_prefix("os_prealloc").unwrap();
        let path = dir.path().join("sub/dir/f");
        let f = open_read_write(&path, DiskCacheMode::Disabled).unwrap();
        write_all_at(&f, 0, b"hello").unwrap();
        preallocate(&f, 1 << 20).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 1 << 20);
        let mut buf = [0u8; 5];
        read_exact_at(&f, 0, &mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        // Never shrinks.
        preallocate(&f, 10).unwrap();
        assert_eq!(f.metadata().unwrap().len(), 1 << 20);

        let n = write_vectored_at(&f, 100, [IoSlice::new(b"ab"), IoSlice::new(b"cd")]).unwrap();
        assert_eq!(n, 4);
        let mut buf = [0u8; 4];
        read_exact_at(&f, 100, &mut buf).unwrap();
        assert_eq!(&buf, b"abcd");

        let copy = dir.path().join("copy");
        let progress = AtomicU64::new(0);
        copy_file_with_progress(&path, &copy, &progress).unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), 1 << 20);
        assert!(same_file(&copy, &copy));
        assert!(!same_file(&copy, &path));
        assert!(available_space(dir.path()).unwrap() > 0);
        assert!(available_space(&dir.path().join("does-not-exist")).is_err());
        let r = open_read(&copy, DiskCacheMode::Disabled).unwrap();
        let mut buf = [0u8; 4];
        read_exact_at(&r, 100, &mut buf).unwrap();
        assert_eq!(&buf, b"abcd");
    }
}
