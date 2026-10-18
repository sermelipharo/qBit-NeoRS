use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchCategory {
    pub id: String,
    pub name: String,
}

/// Installed nova3 search plugin (`/api/v2/search/plugins` item).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct SearchPlugin {
    pub name: String,
    pub full_name: String,
    pub version: String,
    pub url: String,
    pub enabled: bool,
    pub supported_categories: Vec<SearchCategory>,
    /// Local favicon (`data/nova3/engines/<name>.png|.ico`), "" if none.
    /// Not part of the WebAPI JSON.
    #[serde(skip_serializing)]
    pub icon_path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub file_name: String,
    pub file_url: String,
    /// Bytes, -1 unknown.
    pub file_size: i64,
    #[serde(rename = "nbSeeders")]
    pub seeders: i64,
    #[serde(rename = "nbLeechers")]
    pub leechers: i64,
    pub engine_name: String,
    pub site_url: String,
    pub descr_link: String,
    /// Unix seconds, -1 unknown.
    pub pub_date: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SearchJobStatus {
    #[default]
    Running,
    Stopped,
}

/// Detailed job state for the GUI tab (qBittorrent `SearchJobWidget::Status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum SearchJobOutcome {
    /// "Searching..."
    #[default]
    Ongoing,
    /// "Search has finished"
    Finished,
    /// "Search returned no results"
    NoResults,
    /// "Search aborted" (stopped by the user or the 3-minute timeout).
    Aborted,
    /// "An error occurred during search..."
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchJobInfo {
    pub id: i64,
    pub pattern: String,
    pub status: SearchJobStatus,
    pub total: i64,
    /// Not part of the WebAPI JSON.
    #[serde(skip_serializing)]
    pub outcome: SearchJobOutcome,
    /// Error text (search engine stderr) when `outcome` is `Error`.
    /// Not part of the WebAPI JSON.
    #[serde(skip_serializing)]
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchResultsPage {
    pub status: SearchJobStatus,
    pub total: i64,
    pub results: Vec<SearchResult>,
}

/// Whether the search engine can run (needs Python 3.9+).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct SearchEngineStatus {
    pub python_found: bool,
    pub python_path: String,
    pub python_version: String,
    /// Human-readable reason when the engine is unavailable.
    pub message: String,
}
