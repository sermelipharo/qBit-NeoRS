//! `sync/maindata` and `sync/torrentPeers` (qBittorrent `SyncController`).
//!
//! The generic diff (`process_map`, `process_hash`, `process_list`,
//! `generate_sync_data`) works on `serde_json` values exactly like
//! qBittorrent's `QVariantMap` based helpers. `MaindataSync` keeps one
//! snapshot per WebUI session; each poll diffs the current state against it
//! and accumulates the changes until the client acknowledges them by
//! sending back the last `rid`.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

use crate::model::TorrentInfo;

const SUFFIX_REMOVED: &str = "_removed";
/// `rid` values cycle between 1 and this.
const RID_CYCLE: i64 = 1_000_000;

/// Compares `prev` and `data` and returns what changed. Nested maps are
/// diffed recursively; keys listed in `hash_keys` hold "hashes of maps"
/// (new items whole, changed items as diffs, vanished items under
/// `<key>_removed`); arrays are lists of scalars (`<key>` = new items,
/// `<key>_removed` = vanished items, the whole list if it was empty before).
/// Keys that disappear from a flat map are not reported.
pub fn process_map(prev: &Map<String, Value>, data: &Map<String, Value>, hash_keys: &[&str]) -> Map<String, Value> {
    let mut out = Map::new();
    let empty = Map::new();
    for (key, value) in data {
        match value {
            Value::Object(obj) if hash_keys.contains(&key.as_str()) => {
                let prev_obj = prev.get(key).and_then(Value::as_object).unwrap_or(&empty);
                let (changed, removed) = process_hash(prev_obj, obj, hash_keys);
                if !changed.is_empty() {
                    out.insert(key.clone(), Value::Object(changed));
                }
                if !removed.is_empty() {
                    out.insert(format!("{key}{SUFFIX_REMOVED}"), Value::Array(removed));
                }
            }
            Value::Object(obj) => {
                let prev_obj = prev.get(key).and_then(Value::as_object).unwrap_or(&empty);
                let diff = process_map(prev_obj, obj, hash_keys);
                if !diff.is_empty() {
                    out.insert(key.clone(), Value::Object(diff));
                }
            }
            Value::Array(list) => {
                let prev_list = prev.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
                let (added, removed) = process_list(prev_list, list);
                if !added.is_empty() {
                    out.insert(key.clone(), Value::Array(added));
                }
                if !removed.is_empty() {
                    out.insert(format!("{key}{SUFFIX_REMOVED}"), Value::Array(removed));
                }
            }
            scalar => {
                if prev.get(key).unwrap_or(&Value::Null) != scalar || !prev.contains_key(key) && scalar.is_null() {
                    out.insert(key.clone(), scalar.clone());
                }
            }
        }
    }
    out
}

/// "Hash of maps" diff: returns (changed items, removed keys).
pub fn process_hash(
    prev: &Map<String, Value>,
    data: &Map<String, Value>,
    hash_keys: &[&str],
) -> (Map<String, Value>, Vec<Value>) {
    let mut changed = Map::new();
    if prev.is_empty() {
        return (data.clone(), Vec::new());
    }
    for (key, value) in data {
        match (prev.get(key), value) {
            (None, v) => {
                changed.insert(key.clone(), v.clone());
            }
            (Some(Value::Object(p)), Value::Object(n)) => {
                let diff = process_map(p, n, hash_keys);
                if !diff.is_empty() {
                    changed.insert(key.clone(), Value::Object(diff));
                }
            }
            (Some(Value::Array(p)), Value::Array(n)) => {
                let (added, removed) = process_list(p, n);
                if !added.is_empty() || !removed.is_empty() {
                    changed.insert(key.clone(), value.clone());
                }
            }
            (Some(p), n) => {
                if p != n {
                    changed.insert(key.clone(), n.clone());
                }
            }
        }
    }
    let removed = prev.keys().filter(|k| !data.contains_key(*k)).map(|k| Value::String(k.clone())).collect();
    (changed, removed)
}

/// List of scalars diff: returns (added items, removed items).
pub fn process_list(prev: &[Value], data: &[Value]) -> (Vec<Value>, Vec<Value>) {
    if prev.is_empty() {
        return (data.to_vec(), Vec::new());
    }
    let mut remaining: Vec<&Value> = prev.iter().collect();
    let mut added = Vec::new();
    for item in data {
        if let Some(pos) = remaining.iter().position(|p| *p == item) {
            remaining.remove(pos);
        } else {
            added.push(item.clone());
        }
    }
    (added, remaining.into_iter().cloned().collect())
}

/// qBittorrent's `generateSyncData` (used by `sync/torrentPeers`): one
/// last/accepted pair; `rid` equal to the last response accepts it, `rid`
/// equal to the accepted response gives a diff, anything else a full update.
pub fn generate_sync_data(
    accepted_rid: i64,
    data: Map<String, Value>,
    last_accepted: &mut Map<String, Value>,
    last: &mut Map<String, Value>,
    hash_keys: &[&str],
) -> Map<String, Value> {
    let rid_of = |m: &Map<String, Value>| m.get("rid").and_then(Value::as_i64).unwrap_or(0);
    let last_rid = if accepted_rid > 0 { rid_of(last) } else { 0 };
    let mut full = true;
    if last_rid > 0 {
        if last_rid == accepted_rid {
            *last_accepted = last.clone();
        }
        if rid_of(last_accepted) == accepted_rid {
            full = false;
        }
    }
    let mut sync = if full {
        last_accepted.clear();
        let mut s = data.clone();
        s.insert("full_update".into(), Value::Bool(true));
        s
    } else {
        process_map(last_accepted, &data, hash_keys)
    };
    let rid = (last_rid % RID_CYCLE) + 1;
    *last = data;
    last.insert("rid".into(), json!(rid));
    sync.insert("rid".into(), json!(rid));
    sync
}

/// Everything `sync/maindata` reports, gathered from the facade.
#[derive(Default)]
pub struct MaindataInput {
    pub torrents: Vec<TorrentInfo>,
    /// Category name -> WebAPI category object.
    pub categories: Vec<(String, Value)>,
    pub tags: Vec<String>,
    /// Tracker URL -> torrent IDs.
    pub trackers: BTreeMap<String, Vec<String>>,
    pub server_state: Map<String, Value>,
}

#[derive(Default)]
struct Snapshot {
    torrents: HashMap<String, TorrentInfo>,
    categories: Map<String, Value>,
    tags: Vec<String>,
    trackers: BTreeMap<String, Vec<String>>,
    server_state: Map<String, Value>,
}

/// Changes not yet acknowledged by the client.
#[derive(Default)]
struct SyncBuf {
    torrents: Map<String, Value>,
    removed_torrents: Vec<String>,
    categories: Map<String, Value>,
    removed_categories: Vec<String>,
    tags: Vec<String>,
    removed_tags: Vec<String>,
    trackers: Map<String, Value>,
    removed_trackers: Vec<String>,
    server_state: Map<String, Value>,
}

/// Torrent object in maindata: the `torrents/info` object without `hash`.
pub fn maindata_torrent(t: &TorrentInfo) -> Map<String, Value> {
    let mut m = match serde_json::to_value(t) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    m.shift_remove("hash");
    for k in ["dl_limit", "up_limit"] {
        if let Some(v) = m.get_mut(k)
            && v.as_i64().is_some_and(|x| x < 0)
        {
            *v = json!(0);
        }
    }
    m
}

fn merge_into(target: &mut Map<String, Value>, key: &str, diff: Map<String, Value>) {
    match target.get_mut(key) {
        Some(Value::Object(existing)) => {
            for (k, v) in diff {
                existing.insert(k, v);
            }
        }
        _ => {
            target.insert(key.to_owned(), Value::Object(diff));
        }
    }
}

fn remove_str(list: &mut Vec<String>, s: &str) {
    list.retain(|x| x != s);
}

fn push_unique(list: &mut Vec<String>, s: &str) {
    if !list.iter().any(|x| x == s) {
        list.push(s.to_owned());
    }
}

/// Per-session `sync/maindata` state.
pub struct MaindataSync {
    last_sent_id: i64,
    /// -1 until the first call built the snapshot.
    accepted_id: i64,
    snapshot: Snapshot,
    buf: SyncBuf,
}

impl Default for MaindataSync {
    fn default() -> Self {
        Self { last_sent_id: 0, accepted_id: -1, snapshot: Snapshot::default(), buf: SyncBuf::default() }
    }
}

impl MaindataSync {
    /// Handles one `sync/maindata` call with the client's `rid`.
    pub fn poll(&mut self, rid: i64, input: MaindataInput) -> Value {
        let first = self.accepted_id < 0;
        if first {
            self.accepted_id = 0;
        }
        let mut full = true;
        if rid > 0 && self.last_sent_id > 0 {
            if self.last_sent_id == rid {
                self.accepted_id = rid;
                self.buf = SyncBuf::default();
            }
            if self.accepted_id == rid {
                full = false;
            }
        }
        if first {
            self.take_snapshot(input);
        } else {
            self.accumulate(input);
        }
        let id = (self.last_sent_id % RID_CYCLE) + 1;
        let out = self.response(id, full);
        self.last_sent_id = id;
        out
    }

    fn take_snapshot(&mut self, input: MaindataInput) {
        self.snapshot = Snapshot {
            torrents: input.torrents.into_iter().map(|t| (t.hash.clone(), t)).collect(),
            categories: input.categories.into_iter().collect(),
            tags: input.tags,
            trackers: input.trackers,
            server_state: input.server_state,
        };
    }

    /// Diffs `input` against the snapshot into the change buffer.
    fn accumulate(&mut self, input: MaindataInput) {
        let snap = &mut self.snapshot;
        let buf = &mut self.buf;

        // Categories
        let mut seen = Vec::with_capacity(input.categories.len());
        for (name, cat) in input.categories {
            seen.push(name.clone());
            match snap.categories.get(&name) {
                Some(Value::Object(old)) => {
                    if let Value::Object(new) = &cat {
                        let diff = process_map(old, new, &[]);
                        if !diff.is_empty() {
                            merge_into(&mut buf.categories, &name, diff);
                        }
                    }
                }
                _ => {
                    remove_str(&mut buf.removed_categories, &name);
                    buf.categories.insert(name.clone(), cat.clone());
                }
            }
            snap.categories.insert(name, cat);
        }
        let gone: Vec<String> = snap.categories.keys().filter(|k| !seen.contains(k)).cloned().collect();
        for name in gone {
            snap.categories.shift_remove(&name);
            buf.categories.shift_remove(&name);
            push_unique(&mut buf.removed_categories, &name);
        }

        // Tags
        for tag in &input.tags {
            if !snap.tags.contains(tag) {
                remove_str(&mut buf.removed_tags, tag);
                push_unique(&mut buf.tags, tag);
            }
        }
        for tag in &snap.tags {
            if !input.tags.contains(tag) {
                remove_str(&mut buf.tags, tag);
                push_unique(&mut buf.removed_tags, tag);
            }
        }
        snap.tags = input.tags;

        // Torrents
        let mut current = HashMap::with_capacity(input.torrents.len());
        for t in input.torrents {
            match snap.torrents.get(&t.hash) {
                Some(old) if *old == t => {}
                Some(old) => {
                    let diff = process_map(&maindata_torrent(old), &maindata_torrent(&t), &[]);
                    if !diff.is_empty() {
                        merge_into(&mut buf.torrents, &t.hash, diff);
                    }
                }
                None => {
                    remove_str(&mut buf.removed_torrents, &t.hash);
                    buf.torrents.insert(t.hash.clone(), Value::Object(maindata_torrent(&t)));
                }
            }
            current.insert(t.hash.clone(), t);
        }
        for hash in snap.torrents.keys() {
            if !current.contains_key(hash) {
                buf.torrents.shift_remove(hash);
                push_unique(&mut buf.removed_torrents, hash);
            }
        }
        snap.torrents = current;

        // Trackers: changed entries carry the complete list.
        for (url, ids) in &input.trackers {
            let changed = match snap.trackers.get(url) {
                None => true,
                Some(old) => {
                    let (a, r) = process_list(&to_values(old), &to_values(ids));
                    !a.is_empty() || !r.is_empty()
                }
            };
            if changed {
                remove_str(&mut buf.removed_trackers, url);
                buf.trackers.insert(url.clone(), json!(ids));
            }
        }
        for url in snap.trackers.keys() {
            if !input.trackers.contains_key(url) {
                buf.trackers.shift_remove(url);
                push_unique(&mut buf.removed_trackers, url);
            }
        }
        snap.trackers = input.trackers;

        // Server state
        let diff = process_map(&snap.server_state, &input.server_state, &[]);
        for (k, v) in diff {
            buf.server_state.insert(k, v);
        }
        snap.server_state = input.server_state;
    }

    fn response(&mut self, id: i64, full: bool) -> Value {
        let mut out = Map::new();
        out.insert("rid".into(), json!(id));
        if full {
            let snap = &self.snapshot;
            self.buf = SyncBuf {
                torrents: snap.torrents.iter().map(|(h, t)| (h.clone(), Value::Object(maindata_torrent(t)))).collect(),
                categories: snap.categories.clone(),
                tags: snap.tags.clone(),
                trackers: snap.trackers.iter().map(|(u, ids)| (u.clone(), json!(ids))).collect(),
                server_state: snap.server_state.clone(),
                ..Default::default()
            };
            out.insert("full_update".into(), json!(true));
        }
        let b = &self.buf;
        let mut put_map = |k: &str, m: &Map<String, Value>| {
            if !m.is_empty() {
                out.insert(k.into(), Value::Object(m.clone()));
            }
        };
        put_map("categories", &b.categories);
        put_map("torrents", &b.torrents);
        put_map("trackers", &b.trackers);
        put_map("server_state", &b.server_state);
        for (k, list) in [
            ("categories_removed", &b.removed_categories),
            ("tags", &b.tags),
            ("tags_removed", &b.removed_tags),
            ("torrents_removed", &b.removed_torrents),
            ("trackers_removed", &b.removed_trackers),
        ] {
            if !list.is_empty() {
                out.insert(k.into(), json!(list));
            }
        }
        Value::Object(out)
    }
}

fn to_values(list: &[String]) -> Vec<Value> {
    list.iter().map(|s| Value::String(s.clone())).collect()
}

/// Per-session `sync/torrentPeers` state (shared by all torrents, like
/// qBittorrent: switching torrents requires `rid=0`).
#[derive(Default)]
pub struct PeersSync {
    last: Map<String, Value>,
    last_accepted: Map<String, Value>,
}

impl PeersSync {
    pub fn poll(&mut self, rid: i64, data: Map<String, Value>) -> Value {
        Value::Object(generate_sync_data(rid, data, &mut self.last_accepted, &mut self.last, &["peers"]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn process_map_scalars_and_nested() {
        let prev = obj(json!({"a": 1, "b": "x", "c": {"d": 1, "e": 2}, "gone": 5, "n": null}));
        let data = obj(json!({"a": 1, "b": "y", "c": {"d": 1, "e": 3}, "n": null, "new": null}));
        let d = process_map(&prev, &data, &[]);
        assert_eq!(Value::Object(d), json!({"b": "y", "c": {"e": 3}, "new": null}));
        // Unchanged nested map is omitted.
        let d = process_map(&prev, &obj(json!({"c": {"d": 1, "e": 2}})), &[]);
        assert!(d.is_empty());
    }

    #[test]
    fn process_list_semantics() {
        let (a, r) = process_list(&[], &[json!("x"), json!("y")]);
        assert_eq!((a.len(), r.len()), (2, 0));
        let (a, r) = process_list(&[json!("x"), json!("y")], &[json!("y"), json!("z")]);
        assert_eq!(a, vec![json!("z")]);
        assert_eq!(r, vec![json!("x")]);
        let d = process_map(&obj(json!({"l": ["a", "b"]})), &obj(json!({"l": ["b", "c"]})), &[]);
        assert_eq!(Value::Object(d), json!({"l": ["c"], "l_removed": ["a"]}));
    }

    #[test]
    fn process_hash_semantics() {
        let prev = obj(json!({"peers": {"p1": {"x": 1, "y": 1}, "p2": {"x": 1}, "p3": {"x": 1}}}));
        let data = obj(json!({"peers": {"p1": {"x": 2, "y": 1}, "p2": {"x": 1}, "p4": {"x": 9}}}));
        let d = process_map(&prev, &data, &["peers"]);
        assert_eq!(Value::Object(d), json!({"peers": {"p1": {"x": 2}, "p4": {"x": 9}}, "peers_removed": ["p3"]}));
        // Empty previous hash -> everything.
        let d = process_map(&Map::new(), &data, &["peers"]);
        assert_eq!(d["peers"].as_object().unwrap().len(), 3);
    }

    #[test]
    fn generate_sync_data_rid_flow() {
        let (mut acc, mut last) = (Map::new(), Map::new());
        let data1 = obj(json!({"show_flags": true, "peers": {"1.1.1.1:1": {"progress": 0.1}}}));
        let r1 = generate_sync_data(0, data1.clone(), &mut acc, &mut last, &["peers"]);
        assert_eq!(r1["rid"], 1);
        assert_eq!(r1["full_update"], true);
        // Client accepts rid 1 -> diff.
        let data2 = obj(json!({"show_flags": true, "peers": {"1.1.1.1:1": {"progress": 0.2}, "2.2.2.2:2": {"progress": 0.0}}}));
        let r2 = generate_sync_data(1, data2.clone(), &mut acc, &mut last, &["peers"]);
        assert_eq!(r2["rid"], 2);
        assert!(r2.get("full_update").is_none());
        assert!(r2.get("show_flags").is_none());
        assert_eq!(r2["peers"], json!({"1.1.1.1:1": {"progress": 0.2}, "2.2.2.2:2": {"progress": 0.0}}));
        // Client lost response 2 and re-sends 1 -> cumulative diff against 1.
        let data3 = obj(json!({"show_flags": true, "peers": {"2.2.2.2:2": {"progress": 0.0}}}));
        let r3 = generate_sync_data(1, data3, &mut acc, &mut last, &["peers"]);
        assert_eq!(r3["rid"], 3);
        assert_eq!(r3["peers_removed"], json!(["1.1.1.1:1"]));
        assert_eq!(r3["peers"], json!({"2.2.2.2:2": {"progress": 0.0}}));
        // Unknown rid -> full update.
        let r4 = generate_sync_data(77, data2, &mut acc, &mut last, &["peers"]);
        assert_eq!(r4["full_update"], true);
    }

    fn torrent(hash: &str, progress: f64) -> TorrentInfo {
        TorrentInfo { hash: hash.into(), name: hash.to_uppercase(), progress, ..Default::default() }
    }

    fn input(torrents: Vec<TorrentInfo>, tags: &[&str], dl: i64) -> MaindataInput {
        MaindataInput {
            torrents,
            categories: vec![("cat".into(), json!({"name": "cat", "savePath": ""}))],
            tags: tags.iter().map(|s| s.to_string()).collect(),
            trackers: BTreeMap::from([("http://t/a".into(), vec!["aa".into()])]),
            server_state: obj(json!({"dl_info_speed": dl, "connection_status": "connected", "queueing": false})),
        }
    }

    #[test]
    fn maindata_flow() {
        let mut s = MaindataSync::default();
        let r = s.poll(0, input(vec![torrent("aa", 0.1), torrent("bb", 0.2)], &["x"], 5));
        assert_eq!(r["rid"], 1);
        assert_eq!(r["full_update"], true);
        assert_eq!(r["torrents"].as_object().unwrap().len(), 2);
        assert!(r["torrents"]["aa"].get("hash").is_none());
        assert_eq!(r["torrents"]["aa"]["name"], "AA");
        assert_eq!(r["tags"], json!(["x"]));
        assert_eq!(r["server_state"]["dl_info_speed"], 5);
        assert_eq!(r["trackers"], json!({"http://t/a": ["aa"]}));

        // Ack rid 1: only field-level changes.
        let r = s.poll(1, input(vec![torrent("aa", 0.5), torrent("bb", 0.2)], &["x", "y"], 5));
        assert_eq!(r["rid"], 2);
        assert!(r.get("full_update").is_none());
        assert_eq!(r["torrents"], json!({"aa": {"progress": 0.5}}));
        assert_eq!(r["tags"], json!(["y"]));
        assert!(r.get("server_state").is_none());
        assert!(r.get("categories").is_none());

        // Ack rid 2: removal + nested server_state diff + new torrent complete.
        let mut inp = input(vec![torrent("aa", 0.5), torrent("cc", 0.0)], &["y"], 9);
        inp.categories.clear();
        inp.trackers.clear();
        let r = s.poll(2, inp);
        assert_eq!(r["rid"], 3);
        assert_eq!(r["torrents_removed"], json!(["bb"]));
        assert_eq!(r["torrents"]["cc"]["name"], "CC");
        assert!(r["torrents"].get("aa").is_none());
        assert_eq!(r["server_state"], json!({"dl_info_speed": 9}));
        assert_eq!(r["tags_removed"], json!(["x"]));
        assert_eq!(r["categories_removed"], json!(["cat"]));
        assert_eq!(r["trackers_removed"], json!(["http://t/a"]));

        // Client lost response 3 and re-sends 2: cumulative changes since 2.
        let mut inp = input(vec![torrent("aa", 0.7), torrent("cc", 0.0)], &["y"], 9);
        inp.categories.clear();
        inp.trackers.clear();
        let r = s.poll(2, inp);
        assert_eq!(r["rid"], 4);
        assert_eq!(r["torrents_removed"], json!(["bb"]));
        assert_eq!(r["torrents"]["aa"], json!({"progress": 0.7}));
        assert_eq!(r["torrents"]["cc"]["name"], "CC");
        assert_eq!(r["server_state"], json!({"dl_info_speed": 9}));

        // Unknown rid: full update with the current state.
        let mut inp = input(vec![torrent("aa", 0.7)], &[], 9);
        inp.categories.clear();
        let r = s.poll(12345, inp);
        assert_eq!(r["full_update"], true);
        assert_eq!(r["torrents"].as_object().unwrap().len(), 1);
        assert!(r.get("torrents_removed").is_none());
        assert_eq!(r["rid"], 5);
    }

    #[test]
    fn maindata_readded_torrent_is_not_reported_removed() {
        let mut s = MaindataSync::default();
        s.poll(0, input(vec![torrent("aa", 0.1)], &[], 0));
        let r = s.poll(1, input(vec![], &[], 0));
        assert_eq!(r["torrents_removed"], json!(["aa"]));
        // Not acknowledged; torrent comes back.
        let r = s.poll(1, input(vec![torrent("aa", 0.1)], &[], 0));
        assert!(r.get("torrents_removed").is_none());
        assert_eq!(r["torrents"]["aa"]["progress"], 0.1);
    }

    #[test]
    fn rid_wraps() {
        let mut s = MaindataSync { last_sent_id: RID_CYCLE, accepted_id: 0, ..Default::default() };
        let r = s.poll(0, MaindataInput::default());
        assert_eq!(r["rid"], 1);
    }

    #[test]
    fn negative_limits_reported_as_zero() {
        let t = TorrentInfo { hash: "h".into(), dl_limit: -1, up_limit: 5, ..Default::default() };
        let m = maindata_torrent(&t);
        assert_eq!(m["dl_limit"], 0);
        assert_eq!(m["up_limit"], 5);
    }
}
