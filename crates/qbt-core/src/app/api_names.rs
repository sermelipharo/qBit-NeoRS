//! Torrent name rules for the bundled Web UI (qBit NeoRS extension). The
//! macOS app pushes the rules it uses and its name switches; the core keeps
//! them in memory only (the app pushes them again at every start) and serves
//! them at `api/v2/neors/nameRules`.

use std::sync::Arc;

use super::App;
use crate::model::{TorrentNameRule, TorrentNameRules};

/// Bounds of what is kept (the app's own limits: 500 rules per file,
/// patterns up to 300 and tag templates up to 80 UTF-16 units).
pub const MAX_NAME_RULES: usize = 500;
pub const MAX_PATTERN_UNITS: usize = 300;
pub const MAX_TEMPLATE_UNITS: usize = 80;

/// Tag categories the parsers know; anything else becomes `other`.
pub const NAME_CATEGORIES: &[&str] = &[
    "seasonEpisode",
    "volume",
    "resolution",
    "hdr",
    "source",
    "videoCodec",
    "audio",
    "audioLanguage",
    "subtitles",
    "edition",
    "platform",
    "version",
    "region",
    "format",
    "year",
    "releaseGroup",
    "other",
    "hidden",
];

/// The rules in use with a version that changes with their content.
#[derive(Debug, PartialEq, Eq)]
pub struct NameRulesSnapshot {
    pub rules: TorrentNameRules,
    /// 16 hex digits (FNV-1a of the content): equal content, equal version,
    /// also across restarts.
    pub version: String,
}

impl NameRulesSnapshot {
    pub fn new(mut rules: TorrentNameRules) -> Self {
        rules.rules.retain(|r| {
            let units = r.pattern.encode_utf16().count();
            units > 0 && units <= MAX_PATTERN_UNITS && r.template.encode_utf16().count() <= MAX_TEMPLATE_UNITS
        });
        rules.rules.truncate(MAX_NAME_RULES);
        for r in &mut rules.rules {
            if !NAME_CATEGORIES.contains(&r.category.as_str()) {
                r.category = "other".to_owned();
            }
        }
        let version = format!("{:016x}", fingerprint(&rules));
        Self { rules, version }
    }
}

/// FNV-1a over every field (length-prefixed, so fields cannot run together).
fn fingerprint(r: &TorrentNameRules) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    eat(&[u8::from(r.enabled), u8::from(r.semibold_titles), u8::from(r.card_tags), u8::from(r.year_as_tag)]);
    for TorrentNameRule { pattern, category, template, case_sensitive } in &r.rules {
        eat(pattern.as_bytes());
        eat(category.as_bytes());
        eat(template.as_bytes());
        eat(&[u8::from(*case_sensitive)]);
    }
    h
}

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// Replaces the torrent name rules the Web UI uses (in memory; cheap).
    /// Rules over the size limits are dropped, unknown categories become
    /// `other`.
    pub fn set_torrent_name_rules(&self, rules: TorrentNameRules) {
        let next = NameRulesSnapshot::new(rules);
        let mut current = self.name_rules.write();
        if current.as_deref() != Some(&next) {
            *current = Some(Arc::new(next));
        }
    }
}

impl App {
    /// The rules last pushed by the app, if any.
    pub fn torrent_name_rules(&self) -> Option<Arc<NameRulesSnapshot>> {
        self.name_rules.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, category: &str) -> TorrentNameRule {
        TorrentNameRule { pattern: pattern.into(), category: category.into(), template: String::new(), case_sensitive: false }
    }

    #[test]
    fn snapshot_bounds_and_version() {
        let rules = TorrentNameRules {
            rules: vec![rule("1080p", "resolution"), rule("", "other"), rule(&"a".repeat(301), "other"), rule("x", "bogus")],
            enabled: true,
            semibold_titles: true,
            card_tags: true,
            year_as_tag: true,
        };
        let s = NameRulesSnapshot::new(rules.clone());
        assert_eq!(s.rules.rules.len(), 2);
        assert_eq!(s.rules.rules[1].category, "other");
        assert_eq!(s.version.len(), 16);
        assert_eq!(NameRulesSnapshot::new(rules.clone()).version, s.version);
        let mut other = rules;
        other.year_as_tag = false;
        assert_ne!(NameRulesSnapshot::new(other).version, s.version);

        let many = TorrentNameRules { rules: vec![rule("a", "other"); 600], ..Default::default() };
        assert_eq!(NameRulesSnapshot::new(many).rules.rules.len(), MAX_NAME_RULES);
    }
}
