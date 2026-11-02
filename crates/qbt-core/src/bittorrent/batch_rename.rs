//! Batch file renames of the "Manage Torrent Content" dialog (qBittorrent
//! 5.3, `TorrentContentLayoutModel::applyChangedFilePaths`): every changed
//! path is validated with qBittorrent's messages, checked against the layout
//! the batch produces, and the accepted renames are ordered so that renaming
//! the files on disk one after the other never collides (`a → b, b → c` runs
//! `b` first; a swap or `a → a/x` goes through a temporary name).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use super::paths;

/// What to do for a batch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RenamePlan {
    /// `(file index, new path)` in execution order. A file that must make room
    /// for another one first appears twice: once with a temporary name.
    pub steps: Vec<(usize, String)>,
    /// The accepted renames, by file index: their final paths.
    pub accepted: BTreeMap<usize, String>,
    /// Refused requests `(file index, message)`, by file index.
    pub failures: Vec<(usize, String)>,
}

/// Plans renaming the files `current[i]` (relative, `/`-separated) to the
/// requested paths. A request equal to the current path is a no-op; when an
/// index is requested twice, the last request counts.
pub fn plan(current: &[String], requests: &[(i64, String)]) -> RenamePlan {
    let mut failures: BTreeMap<usize, String> = BTreeMap::new();
    let mut accepted: BTreeMap<usize, String> = BTreeMap::new();
    let mut unknown: Vec<(usize, String)> = Vec::new();
    for (index, new) in requests {
        let Some(i) = usize::try_from(*index).ok().filter(|i| *i < current.len()) else {
            unknown.push((usize::MAX, format!("No such file: '{index}'.")));
            continue;
        };
        failures.remove(&i);
        accepted.remove(&i);
        match validate(new) {
            Ok(path) if path == current[i] => {}
            Ok(path) => {
                accepted.insert(i, path);
            }
            Err(message) => {
                failures.insert(i, message);
            }
        }
    }

    reject_conflicts(current, &mut accepted, &mut failures);
    let steps = order(current, &accepted);
    let mut failures: Vec<(usize, String)> = failures.into_iter().collect();
    failures.extend(unknown);
    RenamePlan { steps, accepted, failures }
}

/// The normalized new path, or qBittorrent's message for an invalid one
/// (`TorrentContentHandler::renameFile`).
fn validate(new: &str) -> Result<String, String> {
    let raw = new.trim();
    if raw.starts_with('/') {
        return Err(format!("Absolute path isn't allowed: '{raw}'."));
    }
    let path = paths::normalize_user_path(raw);
    if !paths::is_valid_relative_path(&path) {
        let shown = if path.is_empty() { raw } else { path.as_str() };
        return Err(format!("The new path is invalid: '{shown}'."));
    }
    Ok(path)
}

/// Folders containing `path` (`a/b/c` → `a/b`, `a`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices().rev().filter(|(_, c)| *c == '/').map(move |(i, _)| &path[..i])
}

fn parent(path: &str) -> &str {
    ancestors(path).next().unwrap_or("")
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() { name.to_string() } else { format!("{folder}/{name}") }
}

/// Refuses renames whose path another file of the resulting layout already
/// has, that need a file as a folder, or that would turn a folder into a
/// file. Of several renames to one free path the lowest file index wins.
/// Refusing a rename puts its file back, which can refuse others in turn.
fn reject_conflicts(current: &[String], accepted: &mut BTreeMap<usize, String>, failures: &mut BTreeMap<usize, String>) {
    loop {
        let finals: Vec<&str> = current
            .iter()
            .enumerate()
            .map(|(i, p)| accepted.get(&i).map_or(p.as_str(), String::as_str))
            .collect();
        let mut files: HashMap<&str, Vec<usize>> = HashMap::with_capacity(finals.len());
        let mut folders: HashSet<&str> = HashSet::new();
        for (i, p) in finals.iter().enumerate() {
            files.entry(p).or_default().push(i);
            folders.extend(ancestors(p));
        }
        let mut refused = Vec::new();
        for (&i, path) in accepted.iter() {
            let others = files.get(path.as_str()).map_or(&[][..], Vec::as_slice);
            let duplicate = others.iter().any(|&j| j != i && !(j > i && accepted.contains_key(&j)));
            let folder_clash = folders.contains(path.as_str()) || ancestors(path).any(|a| files.contains_key(a));
            if duplicate || folder_clash {
                refused.push(i);
            }
        }
        if refused.is_empty() {
            return;
        }
        for i in refused {
            if let Some(path) = accepted.remove(&i) {
                failures.insert(i, format!("The file already exists: '{path}'."));
            }
        }
    }
}

/// Orders the accepted renames so that each one finds its target free on
/// disk: a rename waits for the files that sit at its new path, at a folder
/// of it, or inside it (when a folder becomes a file). Files left waiting in
/// a cycle move to a temporary name first.
fn order(current: &[String], accepted: &BTreeMap<usize, String>) -> Vec<(usize, String)> {
    if accepted.is_empty() {
        return Vec::new();
    }
    let mut at_path: HashMap<&str, usize> = HashMap::with_capacity(accepted.len());
    let mut inside: HashMap<&str, Vec<usize>> = HashMap::new();
    for &i in accepted.keys() {
        let path = current[i].as_str();
        at_path.insert(path, i);
        for folder in ancestors(path) {
            inside.entry(folder).or_default().push(i);
        }
    }
    let mut waits_for: BTreeMap<usize, usize> = BTreeMap::new();
    let mut blocks: HashMap<usize, Vec<usize>> = HashMap::new();
    for (&i, new) in accepted {
        let mut blockers: BTreeSet<usize> = BTreeSet::new();
        blockers.extend(at_path.get(new.as_str()));
        blockers.extend(ancestors(new).filter_map(|a| at_path.get(a)));
        blockers.extend(inside.get(new.as_str()).into_iter().flatten());
        waits_for.insert(i, blockers.len());
        for b in blockers {
            blocks.entry(b).or_default().push(i);
        }
    }

    let mut taken = TakenPaths::new(current, accepted);
    let mut ready: BTreeSet<usize> = waits_for.iter().filter(|(_, n)| **n == 0).map(|(i, _)| *i).collect();
    let mut done: HashSet<usize> = HashSet::with_capacity(accepted.len());
    let mut parked: HashSet<usize> = HashSet::new();
    let mut steps = Vec::with_capacity(accepted.len());
    let keys: Vec<usize> = accepted.keys().copied().collect();
    let mut cursor = 0;
    let mut release = |i: usize, waits_for: &mut BTreeMap<usize, usize>, ready: &mut BTreeSet<usize>, done: &HashSet<usize>| {
        for d in blocks.remove(&i).unwrap_or_default() {
            if let Some(n) = waits_for.get_mut(&d) {
                *n -= 1;
                if *n == 0 && !done.contains(&d) {
                    ready.insert(d);
                }
            }
        }
    };
    loop {
        while let Some(i) = ready.pop_first() {
            steps.push((i, accepted[&i].clone()));
            done.insert(i);
            release(i, &mut waits_for, &mut ready, &done);
        }
        while cursor < keys.len() && (done.contains(&keys[cursor]) || parked.contains(&keys[cursor])) {
            cursor += 1;
        }
        let Some(&c) = keys.get(cursor) else { break };
        parked.insert(c);
        steps.push((c, taken.temporary(c, &current[c], &accepted[&c])));
        release(c, &mut waits_for, &mut ready, &done);
    }
    steps
}

/// Paths a temporary name must avoid: every current and final file and folder.
struct TakenPaths<'a> {
    files: HashSet<&'a str>,
    folders: HashSet<&'a str>,
    used: HashSet<String>,
}

impl<'a> TakenPaths<'a> {
    fn new(current: &'a [String], accepted: &'a BTreeMap<usize, String>) -> Self {
        let mut files: HashSet<&str> = HashSet::with_capacity(current.len() + accepted.len());
        let mut folders = HashSet::new();
        for p in current.iter().chain(accepted.values()) {
            files.insert(p.as_str());
            folders.extend(ancestors(p));
        }
        TakenPaths { files, folders, used: HashSet::new() }
    }

    /// A folder that is not, and never was, a file of the batch.
    fn usable_folder(&self, folder: &str) -> bool {
        folder.is_empty() || (!self.files.contains(folder) && ancestors(folder).all(|a| !self.files.contains(a)))
    }

    /// A free name next to the file, else next to its target, else at the top.
    fn temporary(&mut self, index: usize, old: &str, new: &str) -> String {
        let folder = [parent(old), parent(new), ""].into_iter().find(|f| self.usable_folder(f)).unwrap_or("");
        let mut attempt = 0usize;
        loop {
            let name =
                if attempt == 0 { format!(".neors-rename-{index}") } else { format!(".neors-rename-{index}-{attempt}") };
            let path = join(folder, &name);
            if !self.files.contains(path.as_str()) && !self.folders.contains(path.as_str()) && !self.used.contains(&path) {
                self.used.insert(path.clone());
                return path;
            }
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn req(list: &[(i64, &str)]) -> Vec<(i64, String)> {
        list.iter().map(|(i, p)| (*i, p.to_string())).collect()
    }

    /// Applies the steps one at a time, failing like a file system would
    /// when the target is taken (as a file or folder) or needs a file as a folder.
    fn simulate(current: &[String], steps: &[(usize, String)]) -> Vec<String> {
        let mut state = current.to_vec();
        for (i, new) in steps {
            for (j, p) in state.iter().enumerate() {
                if j == *i {
                    continue;
                }
                assert_ne!(p, new, "{new} is taken");
                assert!(!p.starts_with(&format!("{new}/")), "{new} is a folder");
                assert!(!new.starts_with(&format!("{p}/")), "{p} is a file");
            }
            assert!(!new.starts_with(&format!("{}/", state[*i])), "{} can't move into itself", state[*i]);
            state[*i] = new.clone();
        }
        state
    }

    #[test]
    fn simple_renames_and_no_ops() {
        let cur = paths(&["r/a.mkv", "r/b.mkv"]);
        let p = plan(&cur, &req(&[(0, "r/A.mkv"), (1, "r/b.mkv")]));
        assert_eq!(p.steps, vec![(0, "r/A.mkv".to_string())]);
        assert!(p.failures.is_empty());
    }

    #[test]
    fn invalid_paths_use_qbittorrent_messages() {
        let cur = paths(&["a", "b", "c", "d"]);
        let p = plan(&cur, &req(&[(0, "/abs"), (1, "x/../y"), (2, "  "), (3, "ok/name"), (9, "z")]));
        assert_eq!(
            p.failures,
            vec![
                (0, "Absolute path isn't allowed: '/abs'.".to_string()),
                (1, "The new path is invalid: 'x/../y'.".to_string()),
                (2, "The new path is invalid: ''.".to_string()),
                (usize::MAX, "No such file: '9'.".to_string()),
            ]
        );
        assert_eq!(p.steps, vec![(3, "ok/name".to_string())]);
    }

    #[test]
    fn paths_are_normalized() {
        let cur = paths(&["a"]);
        let p = plan(&cur, &req(&[(0, " x//y\\z ")]));
        assert_eq!(p.steps, vec![(0, "x/y/z".to_string())]);
    }

    #[test]
    fn existing_file_or_folder_conflicts() {
        let cur = paths(&["a", "b", "f/c", "g"]);
        let p = plan(&cur, &req(&[(0, "b"), (1, "f"), (3, "a/x")]));
        assert!(p.steps.is_empty());
        assert_eq!(
            p.failures,
            vec![
                (0, "The file already exists: 'b'.".to_string()),
                (1, "The file already exists: 'f'.".to_string()),
                (3, "The file already exists: 'a/x'.".to_string()),
            ]
        );
    }

    #[test]
    fn two_renames_to_one_path_keep_the_first() {
        let cur = paths(&["a", "b"]);
        let p = plan(&cur, &req(&[(1, "c"), (0, "c")]));
        assert_eq!(p.steps, vec![(0, "c".to_string())]);
        assert_eq!(p.failures, vec![(1, "The file already exists: 'c'.".to_string())]);
    }

    #[test]
    fn refusing_one_rename_can_refuse_the_next() {
        // b → c is refused (c stays), so a → b clashes with b staying put.
        let cur = paths(&["a", "b", "c"]);
        let p = plan(&cur, &req(&[(0, "b"), (1, "c")]));
        assert!(p.steps.is_empty());
        assert_eq!(p.failures.len(), 2);
    }

    #[test]
    fn chains_run_in_a_safe_order() {
        let cur = paths(&["a", "b", "c"]);
        let p = plan(&cur, &req(&[(0, "b"), (1, "c"), (2, "d")]));
        assert!(p.failures.is_empty());
        assert_eq!(p.steps, vec![(2, "d".to_string()), (1, "c".to_string()), (0, "b".to_string())]);
        assert_eq!(simulate(&cur, &p.steps), paths(&["b", "c", "d"]));
    }

    #[test]
    fn swaps_go_through_a_temporary_name() {
        let cur = paths(&["d/a", "d/b", "e/c"]);
        let p = plan(&cur, &req(&[(0, "d/b"), (1, "d/a")]));
        assert!(p.failures.is_empty());
        assert_eq!(p.steps.len(), 3);
        assert_eq!(p.steps[0], (0, "d/.neors-rename-0".to_string()));
        assert_eq!(simulate(&cur, &p.steps), paths(&["d/b", "d/a", "e/c"]));
    }

    #[test]
    fn rotations_and_moves_into_own_folder() {
        let cur = paths(&["1", "2", "3", "x"]);
        let p = plan(&cur, &req(&[(0, "2"), (1, "3"), (2, "1"), (3, "x/inner")]));
        assert!(p.failures.is_empty());
        assert_eq!(simulate(&cur, &p.steps), paths(&["2", "3", "1", "x/inner"]));
    }

    #[test]
    fn folder_becomes_file_after_its_files_leave() {
        let cur = paths(&["dir/a", "dir/b", "file"]);
        let p = plan(&cur, &req(&[(2, "dir"), (0, "a"), (1, "b")]));
        assert!(p.failures.is_empty());
        assert_eq!(simulate(&cur, &p.steps), paths(&["a", "b", "dir"]));
        // And a file that becomes a folder waits for the file to leave.
        let cur = paths(&["x", "y"]);
        let p = plan(&cur, &req(&[(1, "x/y"), (0, "z")]));
        assert!(p.failures.is_empty());
        assert_eq!(simulate(&cur, &p.steps), paths(&["z", "x/y"]));
    }

    #[test]
    fn temporary_names_avoid_files_that_become_folders() {
        // The swap's own folder `p` turns into a file, so the temporary name
        // goes next to the target instead.
        let cur = paths(&["p/a", "q/b", "p/c"]);
        let p = plan(&cur, &req(&[(0, "q/b"), (1, "p"), (2, "q/c")]));
        assert!(p.failures.is_empty());
        let finals = simulate(&cur, &p.steps);
        assert_eq!(finals, paths(&["q/b", "p", "q/c"]));
    }

    #[test]
    fn large_batches_stay_linear_enough() {
        let n = 20_000;
        let cur: Vec<String> = (0..n).map(|i| format!("root/season/{i:05}.mkv")).collect();
        let requests: Vec<(i64, String)> = (0..n).map(|i| (i as i64, format!("root/season/E{i:05}.mkv"))).collect();
        let p = plan(&cur, &requests);
        assert!(p.failures.is_empty());
        assert_eq!(p.steps.len(), n);
    }
}
