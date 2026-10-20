import Foundation

/// Stable command identifiers (also the keys of the palette's recent items).
enum CommandID: String, CaseIterable, Sendable {
    // File
    case addTorrentFile, addTorrentLink, importFromQBittorrent, importFromTransmission
    // Torrents
    case start, stop, forceStart, remove, forceRecheck, forceReannounce
    case setLocation, rename, editTrackers, setCategory, editTags, torrentOptions
    case previewFile, openDestination, exportTorrent
    case queueTop, queueUp, queueDown, queueBottom
    case startAll, stopAll, pauseSession, resumeSession
    // View
    case viewAsList, viewAsCards
    case sortByQueue, sortByName, sortByStatus, sortByProgress, sortBySize, sortByDownSpeed, sortByUpSpeed, sortByETA
    case sortByRatio, sortBySeeds, sortByPeers, sortByCategory, sortByAddedOn, sortByCompletedOn, sortByLastActivity
    case sortAscending, sortDescending
    case toggleStatusBar, toggleFiltersSidebar, toggleSpeedInTitle, toggleDetails
    case detailsGeneral, detailsTrackers, detailsPeers, detailsHTTPSources, detailsContent, detailsSpeed
    case resetFilters
    case searchEngine, rssReader, executionLog, statistics
    case lock, setLockPassword
    case globalSpeedLimits, alternativeSpeedLimits
    // Tools and help
    case torrentCreator, manageCookies, options, about, documentation
}
