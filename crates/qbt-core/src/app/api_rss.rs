//! RSS reader and auto-downloader. Mirrors `/api/v2/rss/*`.
//! Item paths use `\` as the folder separator, like qBittorrent.

use super::App;
use crate::error::QbtResult;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// The whole feed tree; `with_articles` includes article lists.
    pub fn rss_tree(&self, with_articles: bool) -> RssFolder {
        self.rss.tree(with_articles)
    }

    /// Articles of a feed or (recursively) a folder, newest first.
    pub fn rss_articles(&self, item_path: String) -> QbtResult<Vec<RssArticle>> {
        self.rss.articles(&item_path)
    }

    pub fn rss_add_folder(&self, path: String) -> QbtResult<()> {
        self.rss.add_folder(&path)
    }

    pub fn rss_add_feed(&self, url: String, path: String, refresh_interval_secs: i64) -> QbtResult<()> {
        self.rss.add_feed(&url, &path, refresh_interval_secs)
    }

    pub fn rss_set_feed_url(&self, path: String, url: String) -> QbtResult<()> {
        self.rss.set_feed_url(&path, &url)
    }

    pub fn rss_set_feed_refresh_interval(&self, path: String, refresh_interval_secs: i64) -> QbtResult<()> {
        self.rss.set_feed_refresh_interval(&path, refresh_interval_secs)
    }

    pub fn rss_remove_item(&self, path: String) -> QbtResult<()> {
        self.rss.remove_item(&path)
    }

    pub fn rss_move_item(&self, item_path: String, dest_path: String) -> QbtResult<()> {
        self.rss.move_item(&item_path, &dest_path)
    }

    /// `article_id` None marks the whole feed/folder as read.
    pub fn rss_mark_as_read(&self, item_path: String, article_id: Option<String>) -> QbtResult<()> {
        self.rss.mark_as_read(&item_path, article_id.as_deref())
    }

    pub fn rss_refresh_item(&self, item_path: String) -> QbtResult<()> {
        self.rss.refresh_item(&item_path)
    }

    pub fn rss_rules(&self) -> Vec<RssAutoDownloadRule> {
        self.rss.rules()
    }

    /// Creates or replaces the rule named `rule.name`.
    pub fn rss_set_rule(&self, rule: RssAutoDownloadRule) -> QbtResult<()> {
        self.rss.set_rule(rule)
    }

    pub fn rss_rename_rule(&self, old_name: String, new_name: String) -> QbtResult<()> {
        self.rss.rename_rule(&old_name, &new_name)
    }

    pub fn rss_remove_rule(&self, name: String) -> QbtResult<()> {
        self.rss.remove_rule(&name)
    }

    pub fn rss_matching_articles(&self, rule_name: String) -> QbtResult<Vec<RssMatchingArticles>> {
        self.rss.matching_articles(&rule_name)
    }

    /// Clears the "previously matched episodes" of a rule.
    pub fn rss_clear_rule_history(&self, rule_name: String) -> QbtResult<()> {
        self.rss.clear_rule_history(&rule_name)
    }

    /// Imports rules exported by qBittorrent (legacy or JSON format).
    pub fn rss_import_rules(&self, data: Vec<u8>) -> QbtResult<()> {
        self.rss.import_rules(&data)
    }

    /// Exports rules as qBittorrent JSON.
    pub fn rss_export_rules(&self) -> Vec<u8> {
        self.rss.export_rules()
    }

    /// Exports rules in qBittorrent's legacy `*.rssrules` format.
    pub fn rss_export_rules_legacy(&self) -> Vec<u8> {
        self.rss.export_rules_legacy()
    }

    /// Matching articles of an unsaved rule (live preview in the RSS
    /// Downloader dialog).
    pub fn rss_preview_rule_matches(&self, rule: RssAutoDownloadRule) -> Vec<RssMatchingArticles> {
        self.rss.matching_articles_for(&rule)
    }

    /// Validates a Must Contain / Must Not Contain expression; empty string
    /// if valid, else the error to show.
    pub fn rss_validate_rule_expression(&self, expression: String, use_regex: bool) -> String {
        crate::rss::validate_rule_expression(&expression, use_regex)
    }

    /// The RSS Downloader dialog's episode filter validation (empty is valid).
    pub fn rss_is_valid_episode_filter(&self, filter: String) -> bool {
        crate::rss::is_valid_episode_filter(&filter)
    }
}

/// Exact WebAPI JSON views used by the WebUI (not exported to Swift).
impl App {
    /// `rss/items` tree.
    pub fn rss_items_json(&self, with_data: bool) -> serde_json::Value {
        self.rss.items_json(with_data)
    }

    /// `rss/rules` object.
    pub fn rss_rules_json(&self) -> serde_json::Value {
        self.rss.rules_json()
    }

    /// `rss/setRule` with the rule definition as sent by the client.
    pub fn rss_set_rule_json(&self, name: String, rule_def: String) -> QbtResult<()> {
        self.rss.set_rule_json(&name, &rule_def)
    }
}
