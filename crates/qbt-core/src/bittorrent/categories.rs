//! Categories (`categories.json`), category name rules, and the save path /
//! download path / share limit resolution of Automatic Torrent Management
//! (`SessionImpl::categorySavePath`, `categoryDownloadPath`,
//! `suggestedSavePath`, `categoryRatioLimit`, ...).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::paths;
use crate::model::{Category, CategoryDownloadPath, ShareLimitAction};

pub type CategoryMap = BTreeMap<String, Category>;

/// `Session::isValidCategoryName`: empty, or no `\`, no leading/trailing `/`
/// and no `//`.
pub fn is_valid_category_name(name: &str) -> bool {
    name.is_empty() || (!name.contains('\\') && !name.starts_with('/') && !name.ends_with('/') && !name.contains("//"))
}

/// Tag validity (`Tag::isValid`): trimmed, non-empty, no `,`.
pub fn is_valid_tag(tag: &str) -> bool {
    let t = tag.trim();
    !t.is_empty() && !t.contains(',') && !t.chars().any(char::is_control)
}

/// `a/b/c` -> `["a", "a/b", "a/b/c"]`.
pub fn expand_category(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut acc = String::new();
    for part in name.split('/') {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        out.push(acc.clone());
    }
    out
}

pub fn parent_category_name(name: &str) -> &str {
    match name.rfind('/') {
        Some(i) => &name[..i],
        None => "",
    }
}

pub fn subcategory_name(name: &str) -> &str {
    match name.rfind('/') {
        Some(i) => &name[i + 1..],
        None => name,
    }
}

/// `belongsToCategory`: equal, or a subcategory of `category`.
pub fn belongs_to_category(torrent_category: &str, category: &str) -> bool {
    if category.is_empty() {
        return torrent_category.is_empty();
    }
    torrent_category == category
        || (torrent_category.len() > category.len()
            && torrent_category.starts_with(category)
            && torrent_category.as_bytes()[category.len()] == b'/')
}

pub fn new_category(name: &str) -> Category {
    Category { name: name.to_string(), ..Default::default() }
}

/// Global path settings that category paths derive from.
#[derive(Clone, Debug, Default)]
pub struct GlobalPaths {
    /// Absolute default save path.
    pub save_path: String,
    /// Absolute global download (incomplete) path.
    pub download_path: String,
    pub download_path_enabled: bool,
}

pub fn category_save_path(cats: &CategoryMap, name: &str, g: &GlobalPaths) -> String {
    if name.is_empty() {
        return g.save_path.clone();
    }
    let opts = cats.get(name);
    let explicit = opts.map(|c| c.save_path.trim()).unwrap_or("");
    if explicit.is_empty() {
        let base = category_save_path(cats, parent_category_name(name), g);
        paths::resolve(&base, &paths::to_valid_path(subcategory_name(name)))
    } else {
        paths::resolve(&g.save_path, explicit)
    }
}

fn resolve_download_option(cats: &CategoryMap, name: &str, option: &CategoryDownloadPath, g: &GlobalPaths) -> (bool, String) {
    if name.is_empty() {
        return (g.download_path_enabled, String::new());
    }
    match option {
        CategoryDownloadPath::Disabled => (false, String::new()),
        CategoryDownloadPath::Path { path } => (true, path.clone()),
        CategoryDownloadPath::Default => {
            let parent = parent_category_name(name);
            let parent_opt = cats.get(parent).map(|c| c.download_path.clone()).unwrap_or_default();
            resolve_download_option(cats, parent, &parent_opt, g)
        }
    }
}

/// Category download path, "" when disabled.
pub fn category_download_path(cats: &CategoryMap, name: &str, g: &GlobalPaths) -> String {
    let option = cats.get(name).map(|c| c.download_path.clone()).unwrap_or_default();
    category_download_path_with(cats, name, &option, g)
}

fn category_download_path_with(cats: &CategoryMap, name: &str, option: &CategoryDownloadPath, g: &GlobalPaths) -> String {
    let (enabled, path) = resolve_download_option(cats, name, option, g);
    if !enabled {
        return String::new();
    }
    if name.is_empty() {
        return g.download_path.clone();
    }
    let path = if path.trim().is_empty() { paths::to_valid_path(subcategory_name(name)) } else { path };
    let expanded = paths::expand_home(path.trim());
    if expanded.starts_with('/') {
        return paths::clean(&expanded);
    }
    let parent = parent_category_name(name);
    // The parent's path is computed as if its download path were enabled.
    let parent_option = match cats.get(parent).map(|c| c.download_path.clone()).unwrap_or_default() {
        CategoryDownloadPath::Path { path } => CategoryDownloadPath::Path { path },
        _ => CategoryDownloadPath::Path { path: String::new() },
    };
    let parent_path = if parent.is_empty() { String::new() } else { category_download_path_with(cats, parent, &parent_option, g) };
    let base = if parent_path.is_empty() { g.download_path.clone() } else { parent_path };
    paths::resolve(&base, &expanded)
}

/// `suggestedSavePath(category, useAutoTMM)`.
pub fn suggested_save_path(
    cats: &CategoryMap,
    category: &str,
    use_auto_tmm: Option<bool>,
    auto_tmm_by_default: bool,
    use_category_paths_in_manual_mode: bool,
    g: &GlobalPaths,
) -> String {
    if use_auto_tmm.unwrap_or(auto_tmm_by_default) || use_category_paths_in_manual_mode {
        category_save_path(cats, category, g)
    } else {
        g.save_path.clone()
    }
}

/// `suggestedDownloadPath(category, useAutoTMM)`.
pub fn suggested_download_path(
    cats: &CategoryMap,
    category: &str,
    use_auto_tmm: Option<bool>,
    auto_tmm_by_default: bool,
    use_category_paths_in_manual_mode: bool,
    g: &GlobalPaths,
) -> String {
    if use_auto_tmm.unwrap_or(auto_tmm_by_default) || use_category_paths_in_manual_mode {
        let p = category_download_path(cats, category, g);
        if !p.is_empty() {
            return p;
        }
    }
    g.download_path.clone()
}

/// Global share limits (BitTorrent page).
#[derive(Clone, Copy, Debug)]
pub struct GlobalShareLimits {
    pub ratio: f64,
    pub seeding_minutes: i64,
    pub inactive_seeding_minutes: i64,
    pub action: ShareLimitAction,
}

pub fn category_ratio_limit(cats: &CategoryMap, name: &str, g: &GlobalShareLimits) -> f64 {
    if name.is_empty() {
        return g.ratio;
    }
    match cats.get(name) {
        Some(c) if c.ratio_limit != -2.0 => c.ratio_limit,
        _ => category_ratio_limit(cats, parent_category_name(name), g),
    }
}

pub fn category_seeding_time_limit(cats: &CategoryMap, name: &str, g: &GlobalShareLimits) -> i64 {
    if name.is_empty() {
        return g.seeding_minutes;
    }
    match cats.get(name) {
        Some(c) if c.seeding_time_limit != -2 => c.seeding_time_limit,
        _ => category_seeding_time_limit(cats, parent_category_name(name), g),
    }
}

pub fn category_inactive_seeding_time_limit(cats: &CategoryMap, name: &str, g: &GlobalShareLimits) -> i64 {
    if name.is_empty() {
        return g.inactive_seeding_minutes;
    }
    match cats.get(name) {
        Some(c) if c.inactive_seeding_time_limit != -2 => c.inactive_seeding_time_limit,
        _ => category_inactive_seeding_time_limit(cats, parent_category_name(name), g),
    }
}

pub fn category_share_limit_action(cats: &CategoryMap, name: &str, g: &GlobalShareLimits) -> ShareLimitAction {
    if name.is_empty() {
        return if g.action == ShareLimitAction::Default { ShareLimitAction::Stop } else { g.action };
    }
    match cats.get(name) {
        Some(c) if c.share_limit_action != ShareLimitAction::Default => c.share_limit_action,
        _ => category_share_limit_action(cats, parent_category_name(name), g),
    }
}

// ---- categories.json ---------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoredCategory {
    #[serde(default)]
    save_path: String,
    /// null = inherit, false = disabled, string = path.
    #[serde(default)]
    download_path: Value,
    #[serde(default = "minus_two_f")]
    ratio_limit: f64,
    #[serde(default = "minus_two")]
    seeding_time_limit: i64,
    #[serde(default = "minus_two")]
    inactive_seeding_time_limit: i64,
    #[serde(default)]
    share_limit_action: String,
}

fn minus_two() -> i64 {
    -2
}
fn minus_two_f() -> f64 {
    -2.0
}

pub fn action_to_str(a: ShareLimitAction) -> &'static str {
    match a {
        ShareLimitAction::Default => "Default",
        ShareLimitAction::Stop => "Stop",
        ShareLimitAction::Remove => "Remove",
        ShareLimitAction::RemoveWithContent => "RemoveWithContent",
        ShareLimitAction::EnableSuperSeeding => "EnableSuperSeeding",
    }
}

pub fn action_from_str(s: &str) -> ShareLimitAction {
    match s {
        "Stop" => ShareLimitAction::Stop,
        "Remove" => ShareLimitAction::Remove,
        "RemoveWithContent" => ShareLimitAction::RemoveWithContent,
        "EnableSuperSeeding" => ShareLimitAction::EnableSuperSeeding,
        _ => ShareLimitAction::Default,
    }
}

/// Serialises categories in qBittorrent's `categories.json` shape.
pub fn to_json(cats: &CategoryMap) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    for (name, c) in cats {
        let stored = StoredCategory {
            save_path: c.save_path.clone(),
            download_path: match &c.download_path {
                CategoryDownloadPath::Default => Value::Null,
                CategoryDownloadPath::Disabled => Value::Bool(false),
                CategoryDownloadPath::Path { path } => Value::String(path.clone()),
            },
            ratio_limit: c.ratio_limit,
            seeding_time_limit: c.seeding_time_limit,
            inactive_seeding_time_limit: c.inactive_seeding_time_limit,
            share_limit_action: action_to_str(c.share_limit_action).to_string(),
        };
        if let Ok(v) = serde_json::to_value(stored) {
            obj.insert(name.clone(), v);
        }
    }
    serde_json::to_vec_pretty(&Value::Object(obj)).unwrap_or_else(|_| b"{}".to_vec())
}

/// Parses `categories.json`. Invalid names are skipped; missing parents are
/// added. Legacy form `{name: "save path"}` is accepted.
pub fn from_json(data: &[u8]) -> Result<CategoryMap, String> {
    let v: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or_else(|| "categories must be a JSON object".to_string())?;
    let mut out = CategoryMap::new();
    for (name, value) in obj {
        if name.is_empty() || !is_valid_category_name(name) {
            continue;
        }
        let mut cat = new_category(name);
        match value {
            Value::String(path) => cat.save_path = path.clone(),
            Value::Object(_) => {
                let Ok(s) = serde_json::from_value::<StoredCategory>(value.clone()) else { continue };
                cat.save_path = s.save_path;
                cat.download_path = match s.download_path {
                    Value::Bool(false) => CategoryDownloadPath::Disabled,
                    Value::String(p) => CategoryDownloadPath::Path { path: p },
                    _ => CategoryDownloadPath::Default,
                };
                cat.ratio_limit = if s.ratio_limit < -2.0 { -1.0 } else { s.ratio_limit };
                cat.seeding_time_limit = s.seeding_time_limit.max(-2);
                cat.inactive_seeding_time_limit = s.inactive_seeding_time_limit.max(-2);
                cat.share_limit_action = action_from_str(&s.share_limit_action);
            }
            _ => {}
        }
        out.insert(name.clone(), cat);
    }
    let names: Vec<String> = out.keys().cloned().collect();
    for name in names {
        for parent in expand_category(&name) {
            out.entry(parent.clone()).or_insert_with(|| new_category(&parent));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g() -> GlobalPaths {
        GlobalPaths { save_path: "/dl".into(), download_path: "/dl/temp".into(), download_path_enabled: false }
    }

    fn cats(entries: &[(&str, &str, CategoryDownloadPath)]) -> CategoryMap {
        entries
            .iter()
            .map(|(n, p, d)| {
                (n.to_string(), Category { save_path: p.to_string(), download_path: d.clone(), ..new_category(n) })
            })
            .collect()
    }

    #[test]
    fn category_names() {
        assert!(is_valid_category_name("a/b"));
        assert!(is_valid_category_name(""));
        assert!(!is_valid_category_name("/a"));
        assert!(!is_valid_category_name("a/"));
        assert!(!is_valid_category_name("a//b"));
        assert!(!is_valid_category_name("a\\b"));
        assert_eq!(expand_category("a/b/c"), vec!["a", "a/b", "a/b/c"]);
        assert!(belongs_to_category("a/b", "a"));
        assert!(!belongs_to_category("ab", "a"));
        assert!(belongs_to_category("a", "a"));
        assert!(is_valid_tag(" x "));
        assert!(!is_valid_tag("a,b"));
        assert!(!is_valid_tag("  "));
    }

    #[test]
    fn save_paths_with_subcategories() {
        let c = cats(&[
            ("Movies", "", CategoryDownloadPath::Default),
            ("Movies/HD", "", CategoryDownloadPath::Default),
            ("TV", "/media/tv", CategoryDownloadPath::Default),
            ("TV/Shows", "rel", CategoryDownloadPath::Default),
            ("Bad", "", CategoryDownloadPath::Default),
        ]);
        let g = g();
        assert_eq!(category_save_path(&c, "", &g), "/dl");
        assert_eq!(category_save_path(&c, "Movies", &g), "/dl/Movies");
        assert_eq!(category_save_path(&c, "Movies/HD", &g), "/dl/Movies/HD");
        assert_eq!(category_save_path(&c, "TV", &g), "/media/tv");
        // Explicit relative paths are relative to the default save path.
        assert_eq!(category_save_path(&c, "TV/Shows", &g), "/dl/rel");
        // Unknown subcategory: implicit under the parent.
        assert_eq!(category_save_path(&c, "TV/New", &g), "/media/tv/New");
        // Dot names cannot escape the parent folder.
        assert_eq!(category_save_path(&c, "..", &g), "/dl/_");
        assert_eq!(category_save_path(&c, "TV/..", &g), "/media/tv/_");
    }

    #[test]
    fn download_paths() {
        let c = cats(&[
            ("A", "", CategoryDownloadPath::Path { path: String::new() }),
            ("A/B", "", CategoryDownloadPath::Default),
            ("C", "", CategoryDownloadPath::Disabled),
            ("D", "", CategoryDownloadPath::Path { path: "/inc/d".into() }),
            ("E", "", CategoryDownloadPath::Default),
        ]);
        let mut g = g();
        assert_eq!(category_download_path(&c, "A", &g), "/dl/temp/A");
        assert_eq!(category_download_path(&c, "A/B", &g), "/dl/temp/A/B");
        assert_eq!(category_download_path(&c, "C", &g), "");
        assert_eq!(category_download_path(&c, "D", &g), "/inc/d");
        assert_eq!(category_download_path(&c, "E", &g), "");
        g.download_path_enabled = true;
        assert_eq!(category_download_path(&c, "E", &g), "/dl/temp/E");
        assert_eq!(category_download_path(&c, "", &g), "/dl/temp");
    }

    #[test]
    fn suggested_paths_manual_and_auto() {
        let c = cats(&[("Movies", "/m", CategoryDownloadPath::Default)]);
        let g = g();
        assert_eq!(suggested_save_path(&c, "Movies", Some(true), false, false, &g), "/m");
        assert_eq!(suggested_save_path(&c, "Movies", Some(false), true, false, &g), "/dl");
        assert_eq!(suggested_save_path(&c, "Movies", None, false, false, &g), "/dl");
        assert_eq!(suggested_save_path(&c, "Movies", None, true, false, &g), "/m");
        // "Use category paths in manual mode".
        assert_eq!(suggested_save_path(&c, "Movies", Some(false), false, true, &g), "/m");
        assert_eq!(suggested_download_path(&c, "Movies", Some(true), false, false, &g), "/dl/temp");
    }

    #[test]
    fn share_limit_inheritance() {
        let mut c = cats(&[("A", "", CategoryDownloadPath::Default), ("A/B", "", CategoryDownloadPath::Default)]);
        c.get_mut("A").unwrap().ratio_limit = 2.0;
        c.get_mut("A").unwrap().share_limit_action = ShareLimitAction::Remove;
        let g = GlobalShareLimits { ratio: 1.0, seeding_minutes: 60, inactive_seeding_minutes: -1, action: ShareLimitAction::Stop };
        assert_eq!(category_ratio_limit(&c, "A/B", &g), 2.0);
        assert_eq!(category_ratio_limit(&c, "", &g), 1.0);
        assert_eq!(category_seeding_time_limit(&c, "A/B", &g), 60);
        assert_eq!(category_share_limit_action(&c, "A/B", &g), ShareLimitAction::Remove);
    }

    #[test]
    fn json_round_trip() {
        let mut c = cats(&[
            ("A", "/a", CategoryDownloadPath::Disabled),
            ("A/B", "", CategoryDownloadPath::Path { path: "x".into() }),
            ("C", "", CategoryDownloadPath::Default),
        ]);
        c.get_mut("C").unwrap().seeding_time_limit = 30;
        let back = from_json(&to_json(&c)).unwrap();
        assert_eq!(back, c);
        let legacy = from_json(br#"{"Old":"/old","x/y":{}, "/bad":{}}"#).unwrap();
        assert_eq!(legacy["Old"].save_path, "/old");
        assert!(legacy.contains_key("x"));
        assert!(!legacy.contains_key("/bad"));
    }
}
