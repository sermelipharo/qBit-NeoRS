//! Plugin files on disk: the plugins folder in the profile
//! (`data/plugins`, qBittorrent's `<data>/plugins`), ownership and
//! permission checks, and size-limited reads that never follow symlinks.
//!
//! A plugin runs with the user's rights, so a file that another account
//! could have written must never load: the folder and every file must be
//! owned by the user running the app and not writable by group or others.

use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Largest plugin file (qBittorrent reads at most 1 MiB).
pub const MAX_PLUGIN_SIZE: u64 = 1024 * 1024;

/// File extension of plugins.
pub const PLUGIN_EXTENSION: &str = "lua";

/// Group/other write bits.
const UNSAFE_WRITE_BITS: u32 = 0o022;

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Checks that `meta` belongs to the current user and is not writable by
/// group or others.
fn check_owner_and_mode(meta: &std::fs::Metadata, what: &str, path: &Path) -> Result<(), String> {
    if meta.uid() != euid() {
        return Err(format!("{what} '{}' is not owned by the current user.", path.display()));
    }
    if meta.mode() & UNSAFE_WRITE_BITS != 0 {
        return Err(format!("{what} '{}' is writable by other users.", path.display()));
    }
    Ok(())
}

/// Creates the plugins folder (mode 0700) when missing and checks it and
/// its parent: real directories (not symlinks) owned by the user and not
/// writable by anyone else.
pub fn prepare_folder(dir: &Path) -> Result<(), String> {
    if let Some(parent) = dir.parent() {
        let meta = std::fs::symlink_metadata(parent).map_err(|e| format!("'{}': {e}", parent.display()))?;
        if !meta.is_dir() {
            return Err(format!("'{}' is not a folder.", parent.display()));
        }
        check_owner_and_mode(&meta, "Folder", parent)?;
    }
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut b = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b.create(dir).map_err(|e| format!("Couldn't create folder '{}': {e}", dir.display()))?;
        }
        Err(e) => return Err(format!("'{}': {e}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("'{}': {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("'{}' is not a folder.", dir.display()));
    }
    check_owner_and_mode(&meta, "Plugins folder", dir)
}

/// The plugin ID of a file name (`<id>.lua`), or `None` when the name is
/// not a plugin file name. IDs are file stems, as in qBittorrent; hidden
/// files, path separators and control characters are refused.
pub fn plugin_id(file_name: &str) -> Option<String> {
    let id = file_name.strip_suffix(&format!(".{PLUGIN_EXTENSION}"))?;
    valid_id(id).then(|| id.to_string())
}

/// Whether `id` can name a plugin file.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= 128
        && !id.starts_with('.')
        && !id.contains(['/', '\\', ':'])
        && !id.chars().any(char::is_control)
}

/// Plugin files (`*.lua`) of the folder, sorted by name.
pub fn list_plugin_files(dir: &Path) -> std::io::Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if let Some(id) = plugin_id(&name) {
            out.push((id, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// Reads an installed plugin: never through a symlink, only a regular file
/// owned by the user, not writable by others, at most [`MAX_PLUGIN_SIZE`].
/// The checks use the opened file descriptor, so the file cannot be swapped
/// between the check and the read.
pub fn read_installed(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| describe_open_error(path, e))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a regular file.", path.display()));
    }
    check_owner_and_mode(&meta, "Plugin file", path)?;
    read_limited(file, meta.len(), path)
}

/// Reads a plugin chosen for installation (any readable regular file; a
/// symlink chosen in a file dialog is followed), at most [`MAX_PLUGIN_SIZE`].
pub fn read_source(path: &Path) -> Result<Vec<u8>, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| describe_open_error(path, e))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("'{}' is not a regular file.", path.display()));
    }
    read_limited(file, meta.len(), path)
}

fn describe_open_error(path: &Path, e: std::io::Error) -> String {
    if e.raw_os_error() == Some(libc::ELOOP) {
        format!("'{}' is a symbolic link.", path.display())
    } else {
        format!("Couldn't open file '{}'. {e}", path.display())
    }
}

fn read_limited(file: std::fs::File, len: u64, path: &Path) -> Result<Vec<u8>, String> {
    if len > MAX_PLUGIN_SIZE {
        return Err(format!("File size ({len}) exceeds limit ({MAX_PLUGIN_SIZE})"));
    }
    let mut data = Vec::with_capacity(len as usize);
    file.take(MAX_PLUGIN_SIZE + 1).read_to_end(&mut data).map_err(|e| format!("'{}': {e}", path.display()))?;
    if data.len() as u64 > MAX_PLUGIN_SIZE {
        return Err(format!("File size ({}) exceeds limit ({MAX_PLUGIN_SIZE})", data.len()));
    }
    Ok(data)
}

/// Writes an installed plugin (0600, atomically).
pub fn write_installed(path: &Path, data: &[u8]) -> Result<(), String> {
    crate::utils::fs::write_atomic_mode(path, data, 0o600).map_err(|e| e.to_string())?;
    // Belt and braces: the mode of an existing destination is replaced by
    // the rename, but make sure nothing widened it.
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_dir() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        d
    }

    #[test]
    fn ids_follow_file_names() {
        assert_eq!(plugin_id("EventHandler.lua").as_deref(), Some("EventHandler"));
        assert_eq!(plugin_id("My.Plugin.lua").as_deref(), Some("My.Plugin"));
        assert_eq!(plugin_id("x.py"), None);
        assert_eq!(plugin_id(".hidden.lua"), None);
        assert_eq!(plugin_id(".lua"), None);
        assert_eq!(plugin_id("a\nb.lua"), None);
        assert_eq!(plugin_id("x.LUA"), None);
    }

    #[test]
    fn folder_is_created_private() {
        let d = private_dir();
        let dir = d.path().join("plugins");
        prepare_folder(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        prepare_folder(&dir).unwrap();
    }

    #[test]
    fn writable_folder_is_refused() {
        let d = private_dir();
        let dir = d.path().join("plugins");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let err = prepare_folder(&dir).unwrap_err();
        assert!(err.contains("writable by other users"), "{err}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(prepare_folder(&dir).is_err());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        prepare_folder(&dir).unwrap();
        // A writable parent lets others swap the folder itself.
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(prepare_folder(&dir).is_err());
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn symlinked_folder_is_refused() {
        let d = private_dir();
        let real = d.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = d.path().join("plugins");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(prepare_folder(&link).unwrap_err().contains("not a folder"));
    }

    #[test]
    fn installed_files_are_checked() {
        let d = private_dir();
        let f = d.path().join("a.lua");
        write_installed(&f, b"NAME='a'").unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().mode() & 0o777, 0o600);
        assert_eq!(read_installed(&f).unwrap(), b"NAME='a'");

        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_installed(&f).unwrap_err().contains("writable by other users"));
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o620)).unwrap();
        assert!(read_installed(&f).is_err());
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_installed(&f).is_ok());

        let link = d.path().join("b.lua");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        assert!(read_installed(&link).unwrap_err().contains("symbolic link"));

        let sub = d.path().join("c.lua");
        std::fs::create_dir(&sub).unwrap();
        assert!(read_installed(&sub).is_err());
    }

    #[test]
    fn size_limit() {
        let d = private_dir();
        let f = d.path().join("big.lua");
        std::fs::write(&f, vec![b'-'; MAX_PLUGIN_SIZE as usize + 1]).unwrap();
        assert!(read_source(&f).unwrap_err().contains("exceeds limit"));
        std::fs::write(&f, vec![b'-'; MAX_PLUGIN_SIZE as usize]).unwrap();
        assert!(read_source(&f).is_ok());
    }

    #[test]
    fn lists_only_plugin_files() {
        let d = private_dir();
        for n in ["b.lua", "a.lua", "x.txt", ".a.lua.0123.tmp", ".hidden.lua"] {
            std::fs::write(d.path().join(n), b"").unwrap();
        }
        let ids: Vec<String> = list_plugin_files(d.path()).unwrap().into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
