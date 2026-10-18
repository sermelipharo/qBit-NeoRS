//! OS integration: moving files to the Trash and macOS quarantine
//! ("Mark-of-the-Web", qBittorrent `Utils::OS::applyMarkOfTheWeb`).

use std::path::Path;

use crate::context::CoreContext;
use crate::error::{QbtError, QbtResult};

/// Moves `path` (file or folder) to the user's Trash. Used by the
/// "Move to trash" torrent content removal option.
#[cfg(target_os = "macos")]
pub fn move_to_trash(path: &Path) -> QbtResult<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    // Never let NSFileManager resolve a dangling/relative path against the
    // process working directory.
    if !path.is_absolute() {
        return Err(QbtError::invalid(format!("not an absolute path: {}", path.display())));
    }
    if std::fs::symlink_metadata(path).is_err() {
        return Err(QbtError::not_found(format!("no such file or directory: {}", path.display())));
    }
    let path_str = path
        .to_str()
        .ok_or_else(|| QbtError::invalid(format!("path is not valid UTF-8: {}", path.display())))?;
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|e| QbtError::io(format!("cannot move {} to the trash: {}", path.display(), e.localizedDescription())))
    })
}

#[cfg(not(target_os = "macos"))]
pub fn move_to_trash(path: &Path) -> QbtResult<()> {
    Err(QbtError::unsupported(format!("moving to the trash is not supported on this platform: {}", path.display())))
}

/// Name of the macOS quarantine extended attribute.
pub const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// Builds the `com.apple.quarantine` value: `flags;hex-timestamp;agent;`.
/// Flag 0x0081 marks a file downloaded by an application that the user has
/// not opened yet, which makes Gatekeeper check it on first launch.
pub fn quarantine_value(unix_time: i64, agent: &str) -> String {
    let agent: String = agent.chars().filter(|c| *c != ';' && !c.is_control()).collect();
    format!("0081;{:08x};{agent};", unix_time.max(0))
}

/// Sets the macOS quarantine attribute on a regular file (never follows
/// symlinks). A file that is already quarantined keeps its attribute.
#[cfg(target_os = "macos")]
pub fn set_quarantine(path: &Path) -> QbtResult<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let meta = std::fs::symlink_metadata(path)
        .map_err(|e| QbtError::io(format!("cannot quarantine {}: {e}", path.display())))?;
    if !meta.is_file() {
        // Mirrors qBittorrent: only existing files are marked; a symlink or
        // directory is left alone instead of tagging whatever it points to.
        return Err(QbtError::invalid(format!("cannot quarantine {}: not a regular file", path.display())));
    }
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| QbtError::invalid(format!("path contains a NUL byte: {}", path.display())))?;
    let name = CString::new(QUARANTINE_XATTR).expect("static name");

    // SAFETY: valid NUL-terminated strings; a null buffer with size 0 only
    // queries the attribute size.
    let existing = unsafe {
        libc::getxattr(c_path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW)
    };
    if existing > 0 {
        return Ok(());
    }

    let value = quarantine_value(chrono::Utc::now().timestamp(), "qBit NeoRS");
    // SAFETY: valid NUL-terminated path/name and a buffer of `value.len()` bytes.
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(QbtError::io(format!("cannot quarantine {}: {err}", path.display())));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn set_quarantine(_path: &Path) -> QbtResult<()> {
    Ok(())
}

/// Applies the quarantine attribute when "Enable quarantine for downloaded
/// files" (`mark_of_the_web`) is on. Failures are logged as warnings (never
/// silently ignored) and returned so callers can react.
pub fn apply_mark_of_the_web(ctx: &CoreContext, path: &Path) -> QbtResult<()> {
    if !cfg!(target_os = "macos") || !ctx.prefs.current().advanced.mark_of_the_web {
        return Ok(());
    }
    set_quarantine(path).inspect_err(|e| {
        ctx.logger.warning(format!("Failed to set the quarantine attribute. File: \"{}\". Reason: {e}", path.display()));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarantine_value_format() {
        assert_eq!(quarantine_value(0x5f3d1c2a, "qBit NeoRS"), "0081;5f3d1c2a;qBit NeoRS;");
        assert_eq!(quarantine_value(1, "a;b\n"), "0081;00000001;ab;");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quarantine_roundtrip() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("download.bin");
        std::fs::write(&file, b"data").unwrap();
        set_quarantine(&file).unwrap();
        // Second call keeps the existing attribute.
        set_quarantine(&file).unwrap();

        let c_path = CString::new(file.as_os_str().as_bytes()).unwrap();
        let name = CString::new(QUARANTINE_XATTR).unwrap();
        let mut buf = [0u8; 128];
        let n = unsafe { libc::getxattr(c_path.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0) };
        assert!(n > 0);
        let value = std::str::from_utf8(&buf[..n as usize]).unwrap();
        assert!(value.starts_with("0081;"), "{value}");
        assert!(value.contains("qBit NeoRS"));

        // Directories and symlinks are refused rather than tagged.
        assert!(set_quarantine(dir.path()).is_err());
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(set_quarantine(&link).is_err());
        assert!(set_quarantine(&dir.path().join("missing")).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn trash_rejects_relative_and_missing() {
        assert!(move_to_trash(Path::new("relative/path")).is_err());
        assert!(move_to_trash(Path::new("/nonexistent/qbt-neo-test-file")).is_err());
    }
}
