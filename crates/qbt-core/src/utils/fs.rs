use std::path::Path;

use crate::error::QbtResult;

/// Writes `data` to `path` atomically (temp file + rename) so a crash never
/// leaves a truncated settings/resume file behind. Profile files hold
/// secrets (preferences: SMTP, proxy and DynDNS passwords), so they are
/// private to the user (0600) and written through an unpredictable,
/// exclusively created temporary file.
pub fn write_atomic(path: &Path, data: &[u8]) -> QbtResult<()> {
    write_atomic_mode(path, data, 0o600)
}

/// Free space in bytes on the volume that contains `path` (walks up to the
/// first existing ancestor).
pub fn free_space(path: &str) -> Option<i64> {
    let mut p = Path::new(path);
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid NUL-terminated path and `st` a valid out-pointer.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    Some((st.f_bavail as i64) * (st.f_frsize as i64))
}

/// Like [`write_atomic`], but the temporary file gets an unpredictable name,
/// is created exclusively (`O_EXCL`, so a planted file or symlink is never
/// followed or reused) with Unix permissions `mode`, and is removed if
/// anything fails. Use `0o600` for files holding secrets (cookies).
pub fn write_atomic_mode(path: &Path, data: &[u8], mode: u32) -> QbtResult<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{file_name}.{:016x}.tmp", rand::random::<u64>()));
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_mode_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/secret.json");
        write_atomic_mode(&path, b"one", 0o600).unwrap();
        write_atomic_mode(&path, b"two", 0o600).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // No temporary files left behind.
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn profile_files_are_private_and_never_follow_planted_links() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preferences.json");
        // The old predictable temporary name, planted as a link elsewhere.
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.path().join("preferences.json.tmp")).unwrap();
        write_atomic(&path, b"{\"smtp_password\":\"x\"}").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
