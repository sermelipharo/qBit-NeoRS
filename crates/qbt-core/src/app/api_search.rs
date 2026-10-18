//! Search engine (nova3 Python plugins). Mirrors `/api/v2/search/*`.

use super::App;
use crate::error::QbtResult;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    /// Python interpreter status ("Python is required to use the search engine").
    pub fn search_engine_status(&self) -> SearchEngineStatus {
        self.search.status()
    }

    pub fn search_plugins(&self) -> Vec<SearchPlugin> {
        self.search.plugins()
    }

    /// `sources`: plugin URLs or local .py paths.
    pub async fn search_install_plugins(&self, sources: Vec<String>) -> QbtResult<()> {
        let s = self.search.clone();
        self.spawn_on_rt(async move { s.install_plugins(sources).await }).await
    }

    pub fn search_uninstall_plugins(&self, names: Vec<String>) -> QbtResult<()> {
        self.search.uninstall_plugins(&names)
    }

    pub fn search_enable_plugins(&self, names: Vec<String>, enable: bool) -> QbtResult<()> {
        self.search.enable_plugins(&names, enable)
    }

    /// Checks the official plugin index and updates outdated plugins.
    pub async fn search_update_plugins(&self) -> QbtResult<()> {
        let s = self.search.clone();
        self.spawn_on_rt(async move { s.update_plugins().await }).await
    }

    /// `plugins`: names, or "all" / "enabled". Returns the job id.
    pub fn search_start(&self, pattern: String, plugins: Vec<String>, category: String) -> QbtResult<i64> {
        self.search.start(&pattern, &plugins, &category)
    }

    pub fn search_stop(&self, id: i64) -> QbtResult<()> {
        self.search.stop(id)
    }

    /// Status of one job, or of all jobs when `id` is None.
    pub fn search_status(&self, id: Option<i64>) -> QbtResult<Vec<SearchJobInfo>> {
        self.search.job_status(id)
    }

    /// `limit` 0 = all; negative `offset` counts from the end.
    pub fn search_results(&self, id: i64, limit: i64, offset: i64) -> QbtResult<SearchResultsPage> {
        self.search.results(id, limit, offset)
    }

    pub fn search_delete(&self, id: i64) -> QbtResult<()> {
        self.search.delete(id)
    }

    /// Downloads a result's torrent through its plugin (nova2dl) and adds it.
    pub async fn search_download_torrent(&self, url: String, plugin: String, params: AddTorrentParams) -> QbtResult<()> {
        let s = self.search.clone();
        self.spawn_on_rt(async move { s.download_torrent(&url, &plugin, params).await }).await
    }
}
