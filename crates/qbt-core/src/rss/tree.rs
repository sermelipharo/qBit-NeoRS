//! The RSS item tree (qBittorrent `RSS::Session` item management and
//! `RSS::Item` path helpers): folders and feeds addressed by `\`-separated
//! paths, unique feed URLs and UIDs, and the `feeds.json` format.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value};

use super::feed::{Feed, Uid};

pub(crate) const SEPARATOR: char = '\\';
const MAX_PATH_LEN: usize = 16 * 1024;
const MAX_NAME_LEN: usize = 4 * 1024;
const MAX_TREE_DEPTH: usize = 64;

/// `Item::isValidPath` (`\A[^\\]+(\\[^\\]+)*\z`) hardened: no control
/// characters, no `.`/`..` segments, bounded length and depth.
pub(crate) fn is_valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_LEN
        && path.split(SEPARATOR).count() <= MAX_TREE_DEPTH
        && path.split(SEPARATOR).all(is_valid_name)
}

pub(crate) fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name != "."
        && name != ".."
        && !name.contains(SEPARATOR)
        && !name.chars().any(char::is_control)
}

pub(crate) fn parent_path(path: &str) -> &str {
    path.rfind(SEPARATOR).map_or("", |i| &path[..i])
}

pub(crate) fn relative_name(path: &str) -> &str {
    path.rfind(SEPARATOR).map_or(path, |i| &path[i + 1..])
}

pub(crate) fn join_path(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_owned() } else { format!("{a}{SEPARATOR}{b}") }
}

/// `true` if `path` is `ancestor` or lies below it.
fn is_within(path: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || path == ancestor
        || (path.len() > ancestor.len() && path.starts_with(ancestor) && path[ancestor.len()..].starts_with(SEPARATOR))
}

#[derive(Debug)]
enum Node {
    /// Child names.
    Folder(BTreeSet<String>),
    Feed(Uid),
}

/// Item tree. The root folder has the path `""`.
#[derive(Debug)]
pub(crate) struct Tree {
    nodes: HashMap<String, Node>,
    feeds: HashMap<Uid, Feed>,
    by_url: HashMap<String, Uid>,
}

impl Default for Tree {
    fn default() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(String::new(), Node::Folder(BTreeSet::new()));
        Self { nodes, feeds: HashMap::new(), by_url: HashMap::new() }
    }
}

/// What a path refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ItemRef {
    Folder,
    Feed(Uid),
}

impl Tree {
    pub fn item(&self, path: &str) -> Option<ItemRef> {
        match self.nodes.get(path)? {
            Node::Folder(_) => Some(ItemRef::Folder),
            Node::Feed(uid) => Some(ItemRef::Feed(*uid)),
        }
    }

    pub fn feed(&self, uid: Uid) -> Option<&Feed> {
        self.feeds.get(&uid)
    }

    pub fn feed_mut(&mut self, uid: Uid) -> Option<&mut Feed> {
        self.feeds.get_mut(&uid)
    }

    pub fn feed_by_url(&self, url: &str) -> Option<&Feed> {
        self.by_url.get(url).and_then(|uid| self.feeds.get(uid))
    }

    pub fn feeds(&self) -> impl Iterator<Item = &Feed> {
        self.feeds.values()
    }

    pub fn feeds_mut(&mut self) -> impl Iterator<Item = &mut Feed> {
        self.feeds.values_mut()
    }

    #[cfg(test)]
    pub fn feed_count(&self) -> usize {
        self.feeds.len()
    }

    /// Child names of a folder, sorted.
    pub fn children(&self, folder: &str) -> Vec<String> {
        match self.nodes.get(folder) {
            Some(Node::Folder(c)) => c.iter().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// Feeds at or below `path` (a feed path yields that feed).
    pub fn feeds_under(&self, path: &str) -> Vec<Uid> {
        match self.nodes.get(path) {
            Some(Node::Feed(uid)) => vec![*uid],
            Some(Node::Folder(_)) => {
                let mut out = Vec::new();
                let mut stack = vec![path.to_owned()];
                while let Some(p) = stack.pop() {
                    for name in self.children(&p) {
                        let child = join_path(&p, &name);
                        match self.nodes.get(&child) {
                            Some(Node::Feed(uid)) => out.push(*uid),
                            Some(Node::Folder(_)) => stack.push(child),
                            None => {}
                        }
                    }
                }
                out
            }
            None => Vec::new(),
        }
    }

    /// `Session::prepareItemDest`.
    fn prepare_dest(&self, path: &str) -> Result<(), String> {
        if !is_valid_path(path) {
            return Err(format!("Incorrect RSS Item path: {path}."));
        }
        if self.nodes.contains_key(path) {
            return Err(format!("RSS item with given path already exists: {path}."));
        }
        let parent = parent_path(path);
        match self.nodes.get(parent) {
            Some(Node::Folder(_)) => Ok(()),
            _ => Err(format!("Parent folder doesn't exist: {parent}.")),
        }
    }

    fn attach(&mut self, path: &str, node: Node) {
        if let Some(Node::Folder(children)) = self.nodes.get_mut(parent_path(path)) {
            children.insert(relative_name(path).to_owned());
        }
        self.nodes.insert(path.to_owned(), node);
    }

    pub fn add_folder(&mut self, path: &str) -> Result<(), String> {
        self.prepare_dest(path)?;
        self.attach(path, Node::Folder(BTreeSet::new()));
        Ok(())
    }

    pub fn add_feed(&mut self, uid: Uid, url: &str, path: &str, refresh_interval: i64) -> Result<Uid, String> {
        if self.by_url.contains_key(url) {
            return Err(format!("RSS feed with given URL already exists: {url}."));
        }
        if self.feeds.contains_key(&uid) {
            return Err(format!("Duplicate RSS feed found. UID: \"{uid}\". Error: Configuration seems to be corrupted."));
        }
        self.prepare_dest(path)?;
        self.attach(path, Node::Feed(uid));
        self.feeds.insert(uid, Feed::new(uid, url.to_owned(), path.to_owned(), refresh_interval));
        self.by_url.insert(url.to_owned(), uid);
        Ok(uid)
    }

    pub fn new_uid(&self) -> Uid {
        loop {
            let uid = Uid::generate();
            if !self.feeds.contains_key(&uid) {
                return uid;
            }
        }
    }

    /// `Session::setFeedURL` bookkeeping (the caller handles renaming).
    pub fn set_feed_url(&mut self, uid: Uid, url: &str) -> Result<bool, String> {
        let Some(feed) = self.feeds.get(&uid) else {
            return Err("Feed doesn't exist.".to_owned());
        };
        if feed.url == url {
            return Ok(false);
        }
        if self.by_url.contains_key(url) {
            return Err(format!("RSS feed with given URL already exists: {url}."));
        }
        let old = feed.url.clone();
        self.by_url.remove(&old);
        self.by_url.insert(url.to_owned(), uid);
        if let Some(feed) = self.feeds.get_mut(&uid) {
            feed.url = url.to_owned();
        }
        Ok(true)
    }

    /// `Session::moveItem` (also rename). Descendant paths follow the item.
    pub fn move_item(&mut self, item_path: &str, dest_path: &str) -> Result<(), String> {
        if item_path.is_empty() {
            return Err("Cannot move root folder.".to_owned());
        }
        let Some(node) = self.nodes.get(item_path) else {
            return Err(format!("Item doesn't exist: {item_path}."));
        };
        if item_path == dest_path {
            return Ok(());
        }
        if matches!(node, Node::Folder(_)) && is_within(dest_path, item_path) {
            return Err("Can't move a folder into itself or its subfolders.".to_owned());
        }
        self.prepare_dest(dest_path)?;

        if let Some(Node::Folder(children)) = self.nodes.get_mut(parent_path(item_path)) {
            children.remove(relative_name(item_path));
        }
        if let Some(Node::Folder(children)) = self.nodes.get_mut(parent_path(dest_path)) {
            children.insert(relative_name(dest_path).to_owned());
        }
        let moved: Vec<String> = self.nodes.keys().filter(|p| is_within(p, item_path)).cloned().collect();
        for old in moved {
            let new = format!("{dest_path}{}", &old[item_path.len()..]);
            if let Some(node) = self.nodes.remove(&old) {
                if let Node::Feed(uid) = &node
                    && let Some(feed) = self.feeds.get_mut(uid)
                {
                    feed.path = new.clone();
                }
                self.nodes.insert(new, node);
            }
        }
        Ok(())
    }

    /// `Session::removeItem`; returns the removed feeds.
    pub fn remove_item(&mut self, path: &str) -> Result<Vec<Feed>, String> {
        if path.is_empty() {
            return Err("Cannot delete root folder.".to_owned());
        }
        if !self.nodes.contains_key(path) {
            return Err(format!("Item doesn't exist: {path}."));
        }
        if let Some(Node::Folder(children)) = self.nodes.get_mut(parent_path(path)) {
            children.remove(relative_name(path));
        }
        let removed: Vec<String> = self.nodes.keys().filter(|p| is_within(p, path)).cloned().collect();
        let mut feeds = Vec::new();
        for p in removed {
            if let Some(Node::Feed(uid)) = self.nodes.remove(&p)
                && let Some(feed) = self.feeds.remove(&uid)
            {
                self.by_url.remove(&feed.url);
                feeds.push(feed);
            }
        }
        Ok(feeds)
    }

    /// `Folder::toJsonValue` for `feeds.json` / WebAPI: names sorted.
    pub fn folder_json(&self, folder: &str, feed_json: &dyn Fn(&Feed) -> Value) -> Value {
        let mut m = Map::new();
        for name in self.children(folder) {
            let child = join_path(folder, &name);
            let v = match self.nodes.get(&child) {
                Some(Node::Folder(_)) => self.folder_json(&child, feed_json),
                Some(Node::Feed(uid)) => match self.feeds.get(uid) {
                    Some(f) => feed_json(f),
                    None => continue,
                },
                None => continue,
            };
            m.insert(name, v);
        }
        Value::Object(m)
    }

    /// `feeds.json` content (pretty, 4-space indent like Qt).
    pub fn feeds_file(&self) -> Vec<u8> {
        let v = self.folder_json("", &|f| {
            let mut m = Map::new();
            m.insert("uid".into(), f.uid.to_string().into());
            m.insert("url".into(), f.url.clone().into());
            if f.refresh_interval > 0 {
                m.insert("refreshInterval".into(), f.refresh_interval.into());
            }
            Value::Object(m)
        });
        to_pretty_json(&v)
    }

    /// `Session::loadFolder`. Returns whether the file must be re-saved
    /// (legacy format, missing UIDs) and the warnings to log.
    pub fn load_folder(&mut self, obj: &Map<String, Value>, folder: &str, warnings: &mut Vec<String>) -> bool {
        let mut updated = false;
        for (key, val) in obj {
            let item_path = join_path(folder, key);
            if !is_valid_name(key) || !is_valid_path(&item_path) {
                warnings.push(format!("Couldn't load RSS item. Item: \"{folder}\\{key}\". Invalid data format."));
                continue;
            }
            match val {
                Value::String(url) => {
                    let url = if url.is_empty() { key.clone() } else { url.clone() };
                    let uid = self.new_uid();
                    if let Err(e) = self.add_feed(uid, &url, &item_path, 0) {
                        warnings.push(e);
                    }
                    updated = true;
                }
                Value::Object(o) if o.contains_key("url") => {
                    let Some(url) = o.get("url").and_then(Value::as_str) else {
                        warnings.push(format!("Couldn't load RSS feed. Feed: \"{folder}\\{key}\". Reason: URL is required."));
                        continue;
                    };
                    let uid = match o.get("uid") {
                        Some(v) => match v.as_str().and_then(Uid::parse) {
                            Some(uid) => uid,
                            None => {
                                warnings.push(format!(
                                    "Couldn't load RSS feed. Feed: \"{folder}\\{key}\". Reason: UID is invalid."
                                ));
                                continue;
                            }
                        },
                        None => {
                            updated = true;
                            self.new_uid()
                        }
                    };
                    let interval = o.get("refreshInterval").and_then(Value::as_i64).unwrap_or(0).max(0);
                    if let Err(e) = self.add_feed(uid, url, &item_path, interval) {
                        warnings.push(e);
                    }
                }
                Value::Object(o) => {
                    if self.add_folder(&item_path).is_ok() && self.load_folder(o, &item_path, warnings) {
                        updated = true;
                    }
                }
                _ => warnings.push(format!("Couldn't load RSS item. Item: \"{folder}\\{key}\". Invalid data format.")),
            }
        }
        updated
    }
}

pub(crate) fn to_pretty_json(v: &Value) -> Vec<u8> {
    use serde::Serialize;
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    if v.serialize(&mut ser).is_err() {
        return b"{}".to_vec();
    }
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert!(is_valid_path("a"));
        assert!(is_valid_path("a\\b c\\d"));
        for bad in ["", "\\a", "a\\", "a\\\\b", ".", "a\\..\\b", "a\u{0}b", "a\nb", "a\u{7f}"] {
            assert!(!is_valid_path(bad), "{bad:?}");
        }
        assert!(!is_valid_path(&"x".repeat(MAX_PATH_LEN + 1)));
        assert_eq!(parent_path("a\\b\\c"), "a\\b");
        assert_eq!(parent_path("a"), "");
        assert_eq!(relative_name("a\\b\\c"), "c");
        assert_eq!(relative_name("c"), "c");
        assert_eq!(join_path("", "x"), "x");
        assert_eq!(join_path("a", "x"), "a\\x");
        assert!(is_within("a\\b", "a"));
        assert!(!is_within("ab", "a"));
        assert!(is_within("a", ""));
    }

    #[test]
    fn tree_operations() {
        let mut t = Tree::default();
        t.add_folder("TV").unwrap();
        t.add_folder("TV\\Shows").unwrap();
        assert_eq!(t.add_folder("TV").unwrap_err(), "RSS item with given path already exists: TV.");
        assert_eq!(t.add_folder("X\\Y").unwrap_err(), "Parent folder doesn't exist: X.");
        assert_eq!(t.add_folder("a\\\\b").unwrap_err(), "Incorrect RSS Item path: a\\\\b.");
        let u1 = t.add_feed(t.new_uid(), "https://a/rss", "TV\\Shows\\A", 0).unwrap();
        let u2 = t.add_feed(t.new_uid(), "https://b/rss", "B", 60).unwrap();
        assert_eq!(
            t.add_feed(t.new_uid(), "https://a/rss", "C", 0).unwrap_err(),
            "RSS feed with given URL already exists: https://a/rss."
        );
        assert!(t.add_feed(t.new_uid(), "https://c/rss", "B\\C", 0).is_err(), "feeds are not folders");
        assert_eq!(t.feeds_under("TV"), vec![u1]);
        assert_eq!(t.feeds_under("").len(), 2);

        // Move folder: descendants follow.
        assert_eq!(t.move_item("TV", "TV\\Shows\\X").unwrap_err(), "Can't move a folder into itself or its subfolders.");
        assert_eq!(t.move_item("", "X").unwrap_err(), "Cannot move root folder.");
        assert_eq!(t.move_item("nope", "X").unwrap_err(), "Item doesn't exist: nope.");
        t.add_folder("Media").unwrap();
        t.move_item("TV", "Media\\Television").unwrap();
        assert_eq!(t.feed(u1).unwrap().path, "Media\\Television\\Shows\\A");
        assert_eq!(t.item("Media\\Television\\Shows\\A"), Some(ItemRef::Feed(u1)));
        assert_eq!(t.item("TV\\Shows\\A"), None);
        assert_eq!(t.children(""), vec!["B".to_owned(), "Media".to_owned()]);
        // Rename a feed.
        t.move_item("B", "Bee").unwrap();
        assert_eq!(t.feed(u2).unwrap().path, "Bee");
        assert_eq!(t.feed(u2).unwrap().name(), "Bee");

        // URL change.
        assert!(t.set_feed_url(u2, "https://b2/rss").unwrap());
        assert!(!t.set_feed_url(u2, "https://b2/rss").unwrap());
        assert!(t.set_feed_url(u2, "https://a/rss").is_err());
        assert!(t.feed_by_url("https://b/rss").is_none());
        assert_eq!(t.feed_by_url("https://b2/rss").unwrap().uid, u2);

        // Remove folder recursively.
        assert_eq!(t.remove_item("").unwrap_err(), "Cannot delete root folder.");
        let removed = t.remove_item("Media").unwrap();
        assert_eq!(removed.len(), 1);
        assert!(t.feed(u1).is_none());
        assert!(t.feed_by_url("https://a/rss").is_none());
        assert_eq!(t.children(""), vec!["Bee".to_owned()]);
    }

    #[test]
    fn feeds_json_round_trip_and_legacy() {
        let mut t = Tree::default();
        t.add_folder("TV").unwrap();
        let u1 = t.add_feed(t.new_uid(), "https://a/rss", "TV\\ShowRSS", 900).unwrap();
        t.add_feed(t.new_uid(), "https://b/rss", "Linux", 0).unwrap();
        let data = t.feeds_file();
        let v: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(v["TV"]["ShowRSS"]["refreshInterval"], 900);
        assert!(v["Linux"].get("refreshInterval").is_none());
        assert!(String::from_utf8_lossy(&data).contains("\n    \"Linux\""));

        let mut t2 = Tree::default();
        let mut warnings = Vec::new();
        assert!(!t2.load_folder(v.as_object().unwrap(), "", &mut warnings));
        assert!(warnings.is_empty());
        assert_eq!(t2.feed(u1).unwrap().path, "TV\\ShowRSS");
        assert_eq!(t2.feed(u1).unwrap().refresh_interval, 900);
        assert_eq!(t2.feeds_file(), data);

        let legacy = serde_json::json!({
            "Old": "https://old/rss",
            "https://same/rss": "",
            "NoUid": {"url": "https://nouid/rss"},
            "BadUid": {"uid": "zzz", "url": "https://bad/rss"},
            "NoUrl": {"url": 5},
            "Num": 5,
            "a\\b": {"url": "https://slash/rss"},
            "Dup": {"uid": u1.to_string(), "url": "https://dup1/rss"},
            "Dup2": {"uid": u1.to_string(), "url": "https://dup2/rss"},
        });
        let mut t3 = Tree::default();
        let mut warnings = Vec::new();
        assert!(t3.load_folder(legacy.as_object().unwrap(), "", &mut warnings));
        assert!(t3.feed_by_url("https://old/rss").is_some());
        assert_eq!(t3.feed_by_url("https://same/rss").unwrap().path, "https://same/rss");
        assert!(t3.feed_by_url("https://nouid/rss").is_some());
        assert!(t3.feed_by_url("https://bad/rss").is_none());
        assert!(t3.feed_by_url("https://slash/rss").is_none());
        assert_eq!(t3.feed_count(), 4);
        assert_eq!(warnings.len(), 5, "{warnings:?}");
    }
}
