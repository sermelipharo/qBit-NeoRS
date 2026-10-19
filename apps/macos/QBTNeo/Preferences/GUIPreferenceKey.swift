import Foundation

/// `UserDefaults` keys of every GUI-only preference. The raw values are the
/// qBittorrent 5.2.4 ini keys (docs/spec/preferences.md §1.2, §2.2, §9, §11)
/// so settings map one to one; values use the same types and defaults.
enum GUIPreferenceKey: String, CaseIterable {
    // Behavior page — interface and transfer list
    case colorScheme = "Appearance/ColorScheme"
    case alternatingRowColors = "Preferences/General/AlternatingRowColors"
    case stateColorScope = "GUI/TransferList/StateColorScope"
    /// Replaced by `stateColorScope`; read once to migrate, then removed.
    case legacyUseTorrentStatesColors = "GUI/TransferList/UseTorrentStatesColors"
    case legacyProgressBarFollowsTextColor = "GUI/TransferList/ProgressBarFollowsTextColor"
    case hideZeroValues = "Preferences/General/HideZeroValues"
    case hideZeroComboValues = "Preferences/General/HideZeroComboValues"
    case doubleClickDownloading = "Preferences/Downloads/DblClOnTorDl"
    case doubleClickCompleted = "Preferences/Downloads/DblClOnTorFn"
    case hideZeroStatusFilters = "TransferListFilters/HideZeroStatusFilters"
    case separateTrackerStatusFilter = "TransferListFilters/SeparateTrackerStatusFilter"
    case torrentContentDragEnabled = "Preferences/General/TorrentContentDragEnabled"
    case startupWindowState = "GUI/StartUpWindowState"
    case confirmOnExit = "Preferences/General/ExitConfirm"
    case dontConfirmAutoExit = "ShutdownConfirmDlg/DontConfirmAutoExit"
    case showSpeedInDock = "Preferences/Desktop/ShowSpeedInDock"
    case showMenuBarIcon = "Preferences/Desktop/ShowMacOSMenuBarIcon"
    case menuBarContentMode = "Preferences/Desktop/MenuBarContentMode"
    case menuBarShowsIcon = "Preferences/Desktop/MenuBarShowsIcon"
    case menuBarSpeedDirection = "Preferences/Desktop/MenuBarSpeedDirection"
    case menuBarHidesZeroSpeed = "Preferences/Desktop/MenuBarHidesZeroSpeed"
    case menuBarCountStyle = "Preferences/Desktop/MenuBarCountStyle"
    /// qBit NeoRS only: feed the desktop and Notification Center widgets.
    case updateWidgets = "Preferences/Desktop/UpdateWidgets"
    case preventSuspendWhenDownloading = "Preferences/General/PreventFromSuspendWhenDownloading"
    case preventSuspendWhenSeeding = "Preferences/General/PreventFromSuspendWhenSeeding"
    case statusBarFreeDiskSpace = "Preferences/General/StatusbarFreeDiskSpaceDisplayed"
    case checkForUpdates = "Preferences/Advanced/updateCheck"

    // Downloads page
    case addTorrentDialogEnabled = "AddNewTorrentDialog/Enabled"
    case addTorrentDialogTopLevel = "AddNewTorrentDialog/TopLevel"
    case confirmMergeTrackers = "GUI/ConfirmActions/MergeTrackers"
    case disableRecursiveDownload = "Preferences/Advanced/DisableRecursiveDownload"

    // Advanced page (GUI-only rows)
    case notificationsEnabled = "GUI/Notifications/Enabled"
    case notifyTorrentAdded = "Application/GUI/Notifications/TorrentAdded"
    case confirmRemoveAllTags = "Preferences/Advanced/confirmRemoveAllTags"
    case confirmRemoveTrackerFromAllTorrents = "GUI/ConfirmActions/RemoveTrackerFromAllTorrents"
    case downloadTrackerFavicon = "GUI/DownloadTrackerFavicon"
    case savePathHistoryLength = "AddNewTorrentDialog/SavePathHistoryLength"
    case speedGraphsEnabled = "SpeedWidget/Enabled"

    // Main window (View / Tools menus)
    case statusBarDisplayed = "Preferences/General/StatusbarDisplayed"
    case filtersSidebarVisible = "GUI/MainWindow/FiltersSidebarVisible"
    case speedInTitleBar = "Preferences/General/SpeedInTitleBar"
    case rssEnabled = "GUI/RSSWidget/Enabled"
    case searchEnabled = "Preferences/Search/SearchEnabled"
    case logEnabled = "GUI/Log/Enabled"
    case logTypes = "GUI/Log/Types"
    case autoExitOnCompletion = "Preferences/Downloads/AutoShutDownqBTOnCompletion"
    case autoSuspendOnCompletion = "Preferences/Downloads/AutoSuspendOnCompletion"
    case autoShutdownOnCompletion = "Preferences/Downloads/AutoShutDownOnCompletion"
    case autoRebootOnCompletion = "Preferences/Downloads/AutoRebootOnCompletion"
    case useRegexFilter = "TransferList/UseRegexAsFilteringPattern"
    case selectedStatusFilter = "TransferListFilters/selectedFilterIndex"
    case statusSectionExpanded = "TransferListFilters/statusFilterState"
    case categorySectionExpanded = "TransferListFilters/CategoryFilterState"
    case tagSectionExpanded = "TransferListFilters/TagFilterState"
    case trackerStatusSectionExpanded = "TransferListFilters/TrackerStatusFilterState"
    case trackerSectionExpanded = "TransferListFilters/trackerFilterState"
    case rssSectionExpanded = "GUI/MainWindow/RSSSectionExpanded"
    /// Sidebar sections hidden from its context menu (`SidebarItem.Section` raw values).
    case hiddenSidebarSections = "GUI/MainWindow/HiddenSidebarSections"
    case lastOpenDirectory = "MainWindow/LastDir"
    case transferListSortOrder = "TransferList/SortOrder"
    /// Column order, widths and visibility of the transfer list (`TableColumnLayout` JSON).
    case transferListColumns = "TransferList/Columns"
    /// Column layout of the Trackers tab (`TableColumnLayout` JSON).
    case trackerListColumns = "TorrentProperties/Trackers/Columns"
    /// Column layout of the Peers tab (`TableColumnLayout` JSON).
    case peerListColumns = "TorrentProperties/Peers/Columns"
    /// Column layout of the Content tab (`TableColumnLayout` JSON).
    case contentListColumns = "TorrentProperties/Content/Columns"
    /// Column layout of the search results (`TableColumnLayout` JSON).
    case searchResultColumns = "SearchTab/Columns"
    /// qBit NeoRS only: the transfer list as a table or as cards (`TransferViewMode`).
    case transferViewMode = "GUI/TransferList/ViewMode"

    // Torrent names (qBit NeoRS only)
    case parseTorrentNames = "GUI/TorrentNames/Enabled"
    case torrentNamesSemiboldTitles = "GUI/TorrentNames/SemiboldTitles"
    case torrentNamesCardTags = "GUI/TorrentNames/CardTags"
    case torrentNamesYearAsTag = "GUI/TorrentNames/YearAsTag"
    /// The edited rules (`NameRule` JSON); absent while the built-in rules are used.
    case torrentNameRules = "GUI/TorrentNames/Rules"

    // Properties panel
    case propertiesVisible = "TorrentProperties/Visible"
    case propertiesCurrentTab = "TorrentProperties/CurrentTab"
    case propertiesHeight = "TorrentProperties/SplitterSizes"
    case contentFilterPatternFormat = "GUI/PropertiesWidget/FilterPatternFormat"
    case speedGraphPeriod = "SpeedWidget/period"
    case speedGraphSeries = "SpeedWidget/graph_enable"

    // Add New Torrent dialog
    case rememberLastSavePath = "AddNewTorrentDialog/RememberLastSavePath"
    case defaultCategory = "AddNewTorrentDialog/DefaultCategory"
    case savePathHistory = "AddNewTorrentDialog/SavePathHistory"
    case downloadPathHistory = "AddNewTorrentDialog/DownloadPathHistory"
    case addTorrentFilterPatternFormat = "GUI/AddNewTorrentDialog/FilterPatternFormat"

    // UI lock (the hash is PBKDF2-HMAC-SHA512, never the password)
    case lockPasswordHash = "Locking/password_PBKDF2"
    case locked = "Locking/locked"

    // Small dialogs
    case trackersListURL = "AddTrackersDialog/TrackersListURL"
}
