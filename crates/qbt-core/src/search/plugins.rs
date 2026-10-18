//! Plugin metadata: name validation, category names, `nova2.py
//! --capabilities` XML and the official `versions.txt` index.

use quick_xml::events::Event;

use super::nova::PluginVersion;
use crate::model::{SearchCategory, SearchPlugin};

/// Category ids understood by `nova2.py` (its `Category` enum).
pub(crate) const CATEGORIES: &[&str] = &["all", "anime", "books", "games", "movies", "music", "pictures", "software", "tv"];

/// `SearchPluginManager::categoryFullName()`. Unknown ids fall back to the id.
pub(crate) fn category_full_name(id: &str) -> &str {
    match id {
        "all" => "All categories",
        "anime" => "Anime",
        "books" => "Books",
        "games" => "Games",
        "movies" => "Movies",
        "music" => "Music",
        "pictures" => "Pictures",
        "software" => "Software",
        "tv" => "TV shows",
        other => other,
    }
}

/// Plugin ids are Python module names used in file paths and argv; only
/// `[A-Za-z0-9_]` is accepted (no path separators, dots or option-like
/// prefixes).
pub(crate) fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// An installed plugin as known to the manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginEntry {
    pub name: String,
    pub full_name: String,
    pub version: PluginVersion,
    pub url: String,
    /// Categories reported by the plugin (without "all").
    pub categories: Vec<String>,
    pub enabled: bool,
    pub icon_path: Option<String>,
}

impl PluginEntry {
    /// WebAPI plugin object: `supportedCategories` starts with "all", then the
    /// plugin's categories sorted case-insensitively.
    pub fn to_model(&self) -> SearchPlugin {
        let mut cats: Vec<&String> = self.categories.iter().filter(|c| c.as_str() != "all").collect();
        cats.sort_by_key(|c| c.to_lowercase());
        cats.dedup();
        let supported_categories = std::iter::once("all")
            .chain(cats.into_iter().map(String::as_str))
            .map(|id| SearchCategory { id: id.to_string(), name: category_full_name(id).to_string() })
            .collect();
        SearchPlugin {
            name: self.name.clone(),
            full_name: self.full_name.clone(),
            version: self.version.to_string(),
            url: self.url.clone(),
            enabled: self.enabled,
            supported_categories,
            icon_path: self.icon_path.clone().unwrap_or_default(),
        }
    }
}

/// One engine element of the capabilities document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Capability {
    pub name: String,
    pub full_name: String,
    pub url: String,
    pub categories: Vec<String>,
}

/// Parses `nova2.py --capabilities` output:
/// `<capabilities><id><name/><url/><categories>a b</categories></id>...</capabilities>`.
pub(crate) fn parse_capabilities(xml: &str) -> Result<Vec<Capability>, String> {
    #[derive(PartialEq)]
    enum Field {
        None,
        Name,
        Url,
        Categories,
    }

    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    let mut seen_root = false;
    let mut out: Vec<Capability> = Vec::new();
    let mut current: Option<Capability> = None;
    let mut field = Field::None;
    let mut text = String::new();
    // Fields already filled for the current engine (first occurrence wins).
    let mut filled = [false; 3];

    loop {
        let event = reader.read_event().map_err(|e| format!("invalid capabilities XML: {e}"))?;
        match event {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match depth {
                    0 => {
                        if tag != "capabilities" || seen_root {
                            return Err("invalid capabilities XML: unexpected root element".into());
                        }
                        seen_root = true;
                    }
                    1 => {
                        current = Some(Capability { name: tag, ..Default::default() });
                        filled = [false; 3];
                    }
                    2 => {
                        field = match tag.as_str() {
                            "name" if !filled[0] => Field::Name,
                            "url" if !filled[1] => Field::Url,
                            "categories" if !filled[2] => Field::Categories,
                            _ => Field::None,
                        };
                        text.clear();
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                match depth {
                    0 => {
                        if tag != "capabilities" || seen_root {
                            return Err("invalid capabilities XML: unexpected root element".into());
                        }
                        seen_root = true;
                    }
                    1 => out.push(Capability { name: tag, ..Default::default() }),
                    _ => {}
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                match depth {
                    // A field of the current engine closed.
                    2 => {
                        if let Some(cap) = current.as_mut() {
                            let value = text.trim();
                            match field {
                                Field::Name => {
                                    cap.full_name = value.to_string();
                                    filled[0] = true;
                                }
                                Field::Url => {
                                    cap.url = value.to_string();
                                    filled[1] = true;
                                }
                                Field::Categories => {
                                    cap.categories =
                                        value.split(' ').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect();
                                    filled[2] = true;
                                }
                                Field::None => {}
                            }
                        }
                        field = Field::None;
                        text.clear();
                    }
                    // The engine element closed.
                    1 => {
                        if let Some(cap) = current.take() {
                            out.push(cap);
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if depth == 3 && field != Field::None {
                    let s = t.xml10_content().map_err(|e| format!("invalid capabilities XML: {e}"))?;
                    text.push_str(&s);
                }
            }
            Event::CData(t) => {
                if depth == 3 && field != Field::None {
                    text.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                if depth == 3 && field != Field::None {
                    match r.resolve_char_ref() {
                        Ok(Some(c)) => text.push(c),
                        _ => {
                            let name = String::from_utf8_lossy(&r).into_owned();
                            match quick_xml::escape::resolve_predefined_entity(&name) {
                                Some(v) => text.push_str(v),
                                None => {
                                    text.push('&');
                                    text.push_str(&name);
                                    text.push(';');
                                }
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !seen_root {
        return Err("invalid capabilities XML: no <capabilities> element".into());
    }
    Ok(out)
}

/// Parsed official plugin index (`versions.txt`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VersionIndex {
    pub entries: Vec<(String, PluginVersion)>,
    pub invalid: usize,
}

impl VersionIndex {
    pub fn total(&self) -> usize {
        self.entries.len() + self.invalid
    }
}

/// `SearchPluginManager::parseVersionInfo()`: `name: x.y` per line; blank
/// lines and `#` comments are skipped; anything else counts as invalid.
pub(crate) fn parse_versions_txt(data: &[u8]) -> VersionIndex {
    let mut index = VersionIndex::default();
    for line in data.split(|&b| b == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let parts: Vec<&[u8]> = line.split(|&b| b == b':').collect();
        if parts.len() != 2 {
            index.invalid += 1;
            continue;
        }
        let version = std::str::from_utf8(parts[1]).ok().and_then(|s| PluginVersion::parse(s.trim()));
        let name = String::from_utf8_lossy(parts[0].trim_ascii()).into_owned();
        match version {
            Some(v) if is_valid_plugin_name(&name) => index.entries.push((name, v)),
            _ => index.invalid += 1,
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_names() {
        assert!(is_valid_plugin_name("piratebay"));
        assert!(is_valid_plugin_name("one337x"));
        assert!(is_valid_plugin_name("torrent_cd2"));
        assert!(!is_valid_plugin_name(""));
        assert!(!is_valid_plugin_name("../evil"));
        assert!(!is_valid_plugin_name("a.b"));
        assert!(!is_valid_plugin_name("-h"));
        assert!(!is_valid_plugin_name("a b"));
        assert!(!is_valid_plugin_name("naïve"));
        assert!(!is_valid_plugin_name(&"a".repeat(129)));
    }

    #[test]
    fn capabilities_xml() {
        let xml = r#"<capabilities>
  <eztv>
    <name>EZTV</name>
    <url>https://eztvx.to/</url>
    <categories>tv</categories>
  </eztv>
  <jackett>
    <name>Jackett &amp; Co &#169;</name>
    <url>http://127.0.0.1:9117</url>
    <categories>anime books games movies music software tv</categories>
  </jackett>
  <nocats>
    <name>No categories</name>
    <url>https://x.example</url>
    <categories />
  </nocats>
</capabilities>
"#;
        let caps = parse_capabilities(xml).unwrap();
        assert_eq!(caps.len(), 3);
        assert_eq!(
            caps[0],
            Capability {
                name: "eztv".into(),
                full_name: "EZTV".into(),
                url: "https://eztvx.to/".into(),
                categories: vec!["tv".into()]
            }
        );
        assert_eq!(caps[1].full_name, "Jackett & Co \u{a9}");
        assert_eq!(caps[1].categories.len(), 7);
        assert_eq!(caps[2].categories, Vec::<String>::new());
    }

    #[test]
    fn capabilities_xml_edge_cases() {
        assert_eq!(parse_capabilities("<capabilities />").unwrap(), vec![]);
        assert_eq!(parse_capabilities("<capabilities></capabilities>").unwrap(), vec![]);
        assert!(parse_capabilities("").is_err());
        assert!(parse_capabilities("<other><a/></other>").is_err());
        assert!(parse_capabilities("Traceback (most recent call last):").is_err());
        assert!(parse_capabilities("<capabilities><a><name>x</a></capabilities>").is_err());
        let caps = parse_capabilities("<capabilities><a/><b><name>B</name></b></capabilities>").unwrap();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].name, "a");
        assert_eq!(caps[1].full_name, "B");
        // Extra spaces in categories are ignored.
        let caps = parse_capabilities("<capabilities><a><categories> movies  tv </categories></a></capabilities>").unwrap();
        assert_eq!(caps[0].categories, vec!["movies".to_string(), "tv".to_string()]);
    }

    #[test]
    fn plugin_model_categories() {
        let entry = PluginEntry {
            name: "x".into(),
            full_name: "X".into(),
            version: PluginVersion::new(1, 5),
            url: "https://x".into(),
            categories: vec!["tv".into(), "Anime".into(), "movies".into(), "all".into(), "custom".into()],
            enabled: true,
            icon_path: None,
        };
        let m = entry.to_model();
        assert_eq!(m.version, "1.5");
        let ids: Vec<&str> = m.supported_categories.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["all", "Anime", "custom", "movies", "tv"]);
        assert_eq!(m.supported_categories[0].name, "All categories");
        assert_eq!(m.supported_categories[3].name, "Movies");
        assert_eq!(m.supported_categories[4].name, "TV shows");
        assert_eq!(m.supported_categories[2].name, "custom");
    }

    #[test]
    fn versions_txt() {
        let data = b"# comment\neztv: 1.16\n\n  jackett : 4.3 \r\nlimetorrents: 4.11\n";
        let idx = parse_versions_txt(data);
        assert_eq!(idx.invalid, 0);
        assert_eq!(
            idx.entries,
            vec![
                ("eztv".to_string(), PluginVersion::new(1, 16)),
                ("jackett".to_string(), PluginVersion::new(4, 3)),
                ("limetorrents".to_string(), PluginVersion::new(4, 11)),
            ]
        );

        let bad = parse_versions_txt(b"a: 1.0\nb 1.0\nc: x.y\nd: 1:2\n../e: 1.0\n");
        assert_eq!(bad.entries.len(), 1);
        assert_eq!(bad.invalid, 4);
        assert_eq!(bad.total(), 5);
        assert_eq!(parse_versions_txt(b""), VersionIndex::default());
    }
}
