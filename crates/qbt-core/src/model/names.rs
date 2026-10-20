//! Torrent name rules (qBit NeoRS extension). The macOS app owns and edits
//! them (GUI defaults); it pushes the rules in use to the core, which only
//! keeps them in memory for the bundled Web UI (`neors/nameRules`), so both
//! front ends split names into the same titles and tags.

use serde::Serialize;

/// One rule of the torrent name parser, as the app uses it (enabled and
/// valid): a regular expression (ICU syntax), the tag category and the tag
/// text template (`$1`…; empty uses the matched text).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct TorrentNameRule {
    pub pattern: String,
    pub category: String,
    pub template: String,
    pub case_sensitive: bool,
}

/// The rules in matching order plus the app's torrent name switches.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct TorrentNameRules {
    pub rules: Vec<TorrentNameRule>,
    /// Names are parsed at all (off: raw names everywhere).
    pub enabled: bool,
    /// The list shows the title part of a name in semibold.
    pub semibold_titles: bool,
    /// Cards show the tags.
    pub card_tags: bool,
    /// The year is a tag (off: appended to the title, "Title (2021)").
    pub year_as_tag: bool,
}
