//! Torrent operations: transfer list, properties tabs, torrent actions,
//! categories and tags. Mirrors `/api/v2/torrents/*` and the transfer-list
//! context menu.

use super::App;
use crate::bittorrent::session::DiscoverySource;
use crate::error::QbtResult;
use crate::model::*;

#[cfg_attr(feature = "ffi", uniffi::export)]
impl App {
    // ---- Transfer list -------------------------------------------------

    /// GUI transfer-list sync: returns what changed since the previous call
    /// (first call, or after [`App::reset_main_data`], is a full update).
    pub fn sync_main_data(&self) -> MainDataUpdate {
        let mut sync = self.gui_sync.lock();
        let snapshot = self.session.main_data_snapshot_since(sync.known_trackers_fp());
        sync.diff(snapshot)
    }

    /// Forces the next [`App::sync_main_data`] to be a full update.
    pub fn reset_main_data(&self) {
        self.gui_sync.lock().reset();
    }

    /// All torrents, unsorted.
    pub fn torrents(&self) -> Vec<TorrentInfo> {
        self.session.torrent_infos()
    }

    /// Torrents matching a filter (status, category, tag, hashes, private).
    pub fn torrents_filtered(&self, query: TorrentQuery) -> Vec<TorrentInfo> {
        self.session.torrent_infos_filtered(&query)
    }

    pub fn torrent(&self, hash: String) -> QbtResult<TorrentInfo> {
        self.session.torrent_info(&hash)
    }

    pub fn torrent_count(&self) -> u64 {
        self.session.torrent_count() as u64
    }

    // ---- Properties tabs ------------------------------------------------

    pub fn properties(&self, hash: String) -> QbtResult<TorrentProperties> {
        self.session.properties(&hash)
    }

    /// Trackers tab, including the DHT/PeX/LSD pseudo rows first.
    pub fn trackers(&self, hash: String) -> QbtResult<Vec<TrackerEntry>> {
        self.session.trackers(&hash)
    }

    pub fn web_seeds(&self, hash: String) -> QbtResult<Vec<WebSeed>> {
        self.session.web_seeds(&hash)
    }

    /// Content tab; `indexes` empty = all files.
    pub fn files(&self, hash: String, indexes: Vec<i64>) -> QbtResult<Vec<TorrentFile>> {
        self.session.files(&hash, &indexes)
    }

    /// One byte per piece: 0 missing, 1 downloading, 2 downloaded.
    pub fn piece_states(&self, hash: String) -> QbtResult<Vec<u8>> {
        self.session.piece_states(&hash)
    }

    /// Number of connected peers having each piece.
    pub fn piece_availability(&self, hash: String) -> QbtResult<Vec<u32>> {
        self.session.piece_availability(&hash)
    }

    /// SHA-1 piece hashes (hex) of a v1/hybrid torrent.
    pub fn piece_hashes(&self, hash: String) -> QbtResult<Vec<String>> {
        self.session.piece_hashes(&hash)
    }

    /// Peers tab.
    pub fn peers(&self, hash: String) -> QbtResult<Vec<PeerInfo>> {
        self.session.peers(&hash)
    }

    /// Exports the .torrent file (bencoded).
    pub fn export_torrent(&self, hash: String) -> QbtResult<Vec<u8>> {
        self.session.export_torrent(&hash)
    }

    // ---- Adding ---------------------------------------------------------

    /// Adds a torrent and returns its hash. For magnets the torrent is added
    /// immediately in the "downloading metadata" state.
    pub async fn add_torrent(&self, source: TorrentSource, params: AddTorrentParams) -> QbtResult<String> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.add_torrent(source, params).await }).await
    }

    /// Adds a torrent downloaded on the user's behalf (its trackers and web
    /// seeds are not trusted by SSRF mitigation).
    pub(crate) async fn add_remote_torrent(&self, source: TorrentSource, params: AddTorrentParams) -> QbtResult<String> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.add_remote_torrent(source, params).await }).await
    }

    /// Parses a .torrent / downloads a URL / resolves a magnet's metadata from
    /// the swarm, without adding it. Cancel by dropping the future.
    pub async fn fetch_metadata(&self, source: TorrentSource) -> QbtResult<TorrentMetadataInfo> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.fetch_metadata(source).await }).await
    }

    /// Bencoded .torrent built from metadata previously fetched with
    /// [`App::fetch_metadata`] (for "Save as .torrent file" in the add dialog).
    pub fn save_fetched_metadata(&self, hash: String) -> QbtResult<Vec<u8>> {
        self.session.fetched_metadata_torrent(&hash)
    }

    // ---- Actions ----------------------------------------------------------

    pub async fn start_torrents(&self, hashes: Vec<String>) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.start(&hashes).await }).await
    }

    pub async fn stop_torrents(&self, hashes: Vec<String>) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.stop(&hashes).await }).await
    }

    pub async fn set_force_start(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.set_force_start(&hashes, value).await }).await
    }

    pub async fn remove_torrents(&self, hashes: Vec<String>, delete_files: bool) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.remove(&hashes, delete_files).await }).await
    }

    pub async fn recheck_torrents(&self, hashes: Vec<String>) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.recheck(&hashes).await }).await
    }

    pub async fn reannounce_torrents(&self, hashes: Vec<String>) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.reannounce(&hashes).await }).await
    }

    /// "Set location": moves content to `location` (disables auto TMM).
    pub async fn set_location(&self, hashes: Vec<String>, location: String) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.set_location(&hashes, &location).await }).await
    }

    pub async fn set_save_path(&self, hashes: Vec<String>, path: String) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.set_save_path(&hashes, &path).await }).await
    }

    pub async fn set_download_path(&self, hashes: Vec<String>, path: String) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.set_download_path(&hashes, &path).await }).await
    }

    pub fn rename_torrent(&self, hash: String, name: String) -> QbtResult<()> {
        self.session.rename(&hash, &name)
    }

    pub fn set_comment(&self, hashes: Vec<String>, comment: String) -> QbtResult<()> {
        self.session.set_comment(&hashes, &comment)
    }

    pub async fn rename_file(&self, hash: String, old_path: String, new_path: String) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.rename_file(&hash, &old_path, &new_path).await }).await
    }

    pub async fn rename_folder(&self, hash: String, old_path: String, new_path: String) -> QbtResult<()> {
        let s = self.session.clone();
        self.spawn_on_rt(async move { s.rename_folder(&hash, &old_path, &new_path).await }).await
    }

    pub fn set_file_priority(&self, hash: String, indexes: Vec<i64>, priority: FilePriority) -> QbtResult<()> {
        self.session.set_file_priority(&hash, &indexes, priority)
    }

    pub fn set_auto_management(&self, hashes: Vec<String>, enabled: bool) -> QbtResult<()> {
        self.session.set_auto_management(&hashes, enabled)
    }

    /// bytes/s, <= 0 = unlimited.
    pub fn set_download_limit(&self, hashes: Vec<String>, limit: i64) -> QbtResult<()> {
        self.session.set_download_limit(&hashes, limit)
    }

    pub fn set_upload_limit(&self, hashes: Vec<String>, limit: i64) -> QbtResult<()> {
        self.session.set_upload_limit(&hashes, limit)
    }

    /// Ratio -2 = global, -1 = unlimited; times in minutes with the same sentinels.
    pub fn set_share_limits(
        &self,
        hashes: Vec<String>,
        ratio_limit: f64,
        seeding_time_limit: i64,
        inactive_seeding_time_limit: i64,
        action: ShareLimitAction,
    ) -> QbtResult<()> {
        self.session
            .set_share_limits(&hashes, ratio_limit, seeding_time_limit, inactive_seeding_time_limit, action)
    }

    pub fn set_super_seeding(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_super_seeding(&hashes, value)
    }

    /// Per-torrent DHT/PeX/LSD switches (Torrent Options dialog).
    pub fn discovery_options(&self, hashes: Vec<String>) -> Vec<TorrentDiscoveryOptions> {
        self.session.discovery_options(&hashes)
    }

    /// "Disable DHT for this torrent" (ignored for private torrents).
    pub fn set_dht_disabled(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_discovery_disabled(&hashes, DiscoverySource::Dht, value)
    }

    /// "Disable PeX for this torrent" (ignored for private torrents).
    pub fn set_pex_disabled(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_discovery_disabled(&hashes, DiscoverySource::Pex, value)
    }

    /// "Disable LSD for this torrent" (ignored for private torrents).
    pub fn set_lsd_disabled(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_discovery_disabled(&hashes, DiscoverySource::Lsd, value)
    }

    pub fn set_sequential_download(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_sequential_download(&hashes, value)
    }

    pub fn set_first_last_piece_priority(&self, hashes: Vec<String>, value: bool) -> QbtResult<()> {
        self.session.set_first_last_piece_priority(&hashes, value)
    }

    // ---- Queue ------------------------------------------------------------

    pub fn queue_increase(&self, hashes: Vec<String>) -> QbtResult<()> {
        self.session.queue_move(&hashes, QueueMove::Up)
    }

    pub fn queue_decrease(&self, hashes: Vec<String>) -> QbtResult<()> {
        self.session.queue_move(&hashes, QueueMove::Down)
    }

    pub fn queue_top(&self, hashes: Vec<String>) -> QbtResult<()> {
        self.session.queue_move(&hashes, QueueMove::Top)
    }

    pub fn queue_bottom(&self, hashes: Vec<String>) -> QbtResult<()> {
        self.session.queue_move(&hashes, QueueMove::Bottom)
    }

    // ---- Trackers, peers, web seeds ---------------------------------------

    pub fn add_trackers(&self, hash: String, trackers: Vec<TrackerTierEntry>) -> QbtResult<()> {
        self.session.add_trackers(&hash, trackers)
    }

    pub fn edit_tracker(&self, hash: String, orig_url: String, new_url: String) -> QbtResult<()> {
        self.session.edit_tracker(&hash, &orig_url, &new_url)
    }

    pub fn remove_trackers(&self, hash: String, urls: Vec<String>) -> QbtResult<()> {
        self.session.remove_trackers(&hash, &urls)
    }

    /// Replaces the whole tracker list (Edit trackers dialog).
    pub fn replace_trackers(&self, hash: String, trackers: Vec<TrackerTierEntry>) -> QbtResult<()> {
        self.session.replace_trackers(&hash, trackers)
    }

    /// `peers` are "ip:port" strings.
    pub fn add_peers(&self, hashes: Vec<String>, peers: Vec<String>) -> QbtResult<()> {
        self.session.add_peers(&hashes, &peers)
    }

    /// Bans peers ("ip:port" or "ip") permanently (adds to banned IPs).
    pub fn ban_peers(&self, peers: Vec<String>) -> QbtResult<()> {
        self.session.ban_peers(&peers)
    }

    pub fn add_web_seeds(&self, hash: String, urls: Vec<String>) -> QbtResult<()> {
        self.session.add_web_seeds(&hash, &urls)
    }

    pub fn edit_web_seed(&self, hash: String, orig_url: String, new_url: String) -> QbtResult<()> {
        self.session.edit_web_seed(&hash, &orig_url, &new_url)
    }

    pub fn remove_web_seeds(&self, hash: String, urls: Vec<String>) -> QbtResult<()> {
        self.session.remove_web_seeds(&hash, &urls)
    }

    // ---- Categories & tags --------------------------------------------------

    pub fn categories(&self) -> Vec<Category> {
        self.session.categories()
    }

    pub fn create_category(&self, category: Category) -> QbtResult<()> {
        self.session.create_category(category)
    }

    pub fn edit_category(&self, category: Category) -> QbtResult<()> {
        self.session.edit_category(category)
    }

    pub fn remove_categories(&self, names: Vec<String>) -> QbtResult<()> {
        self.session.remove_categories(&names)
    }

    pub fn set_category(&self, hashes: Vec<String>, category: String) -> QbtResult<()> {
        self.session.set_category(&hashes, &category)
    }

    pub fn tags(&self) -> Vec<String> {
        self.session.tags()
    }

    pub fn create_tags(&self, tags: Vec<String>) -> QbtResult<()> {
        self.session.create_tags(&tags)
    }

    pub fn delete_tags(&self, tags: Vec<String>) -> QbtResult<()> {
        self.session.delete_tags(&tags)
    }

    pub fn add_tags(&self, hashes: Vec<String>, tags: Vec<String>) -> QbtResult<()> {
        self.session.add_tags(&hashes, &tags)
    }

    /// Empty `tags` removes all tags.
    pub fn remove_tags(&self, hashes: Vec<String>, tags: Vec<String>) -> QbtResult<()> {
        self.session.remove_tags(&hashes, &tags)
    }

    pub fn set_tags(&self, hashes: Vec<String>, tags: Vec<String>) -> QbtResult<()> {
        self.session.set_tags(&hashes, &tags)
    }
}

/// Calls used by the WebAPI that are not exported to Swift.
impl App {
    /// Complete transfer-list state (torrents, categories, tags, trackers,
    /// server state) for `sync/maindata`; the WebUI diffs it per session.
    pub fn main_data_snapshot(&self) -> crate::bittorrent::sync::MainDataSnapshot {
        self.session.main_data_snapshot()
    }
}
