//! Content removal (`TorrentContentRemover`): delete or move to the Trash,
//! then remove empty folders bottom-up together with junk files.

use std::path::{Path, PathBuf};

const JUNK_FILES: [&str; 4] = ["thumbs.db", "desktop.ini", ".directory", ".ds_store"];

/// Moves `path` to the user's Trash (`~/.Trash`, or `<volume>/.Trashes/<uid>`
/// for other volumes) under a unique name.
pub fn move_to_trash(path: &Path) -> std::io::Result<()> {
    let name = path.file_name().ok_or_else(|| std::io::Error::other("invalid path"))?.to_os_string();
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".Trash"));
    }
    if let Some(volume) = volume_root(path) {
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        candidates.push(volume.join(".Trashes").join(uid.to_string()));
    }
    let mut last_err = std::io::Error::other("no trash folder available");
    for trash in candidates {
        if !trash.is_dir() {
            continue;
        }
        let mut target = trash.join(&name);
        let mut n = 1;
        while target.exists() {
            let mut alt = name.clone();
            alt.push(format!(" {n}"));
            target = trash.join(alt);
            n += 1;
        }
        match std::fs::rename(path, &target) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

fn volume_root(path: &Path) -> Option<PathBuf> {
    let mut comps = path.components();
    let mut out = PathBuf::new();
    out.push(comps.next()?);
    let first = comps.next()?;
    if first.as_os_str() == "Volumes" {
        out.push(first);
        out.push(comps.next()?);
        Some(out)
    } else {
        None
    }
}

/// Removes a file permanently or moves it to the Trash.
pub fn remove_file(path: &Path, to_trash: bool) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(m) if m.is_dir() => return Ok(()),
        Ok(_) => {}
    }
    if to_trash {
        move_to_trash(path).or_else(|_| std::fs::remove_file(path))
    } else {
        std::fs::remove_file(path)
    }
}

/// `Utils::Fs::smartRemoveEmptyFolderTree`: removes `dir` and its empty
/// sub-folders (bottom-up) when they contain nothing but junk files.
/// Symlinks are never followed.
pub fn smart_remove_empty_folder_tree(dir: &Path) {
    fn walk(dir: &Path, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return false };
        let mut empty = true;
        let mut junk = Vec::new();
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else {
                empty = false;
                continue;
            };
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            if ft.is_dir() {
                if !walk(&e.path(), depth + 1) {
                    empty = false;
                }
            } else if ft.is_file() && (JUNK_FILES.contains(&name.as_str()) || name.ends_with('~')) {
                junk.push(e.path());
            } else {
                empty = false;
            }
        }
        if !empty {
            return false;
        }
        for j in junk {
            let _ = std::fs::remove_file(j);
        }
        std::fs::remove_dir(dir).is_ok()
    }
    if std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
        walk(dir, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_empty_tree_with_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("t");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/.DS_Store"), b"x").unwrap();
        std::fs::write(root.join("a/b/file~"), b"x").unwrap();
        smart_remove_empty_folder_tree(&root);
        assert!(!root.exists());

        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/data"), b"x").unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        smart_remove_empty_folder_tree(&root);
        assert!(root.join("keep/data").exists());
        assert!(!root.join("empty").exists());
    }

    #[test]
    fn remove_missing_file_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(remove_file(&tmp.path().join("nope"), false).is_ok());
        let f = tmp.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        remove_file(&f, false).unwrap();
        assert!(!f.exists());
        assert_eq!(volume_root(Path::new("/Volumes/Ext/x/y")), Some(PathBuf::from("/Volumes/Ext")));
        assert_eq!(volume_root(Path::new("/Users/x")), None);
    }
}
