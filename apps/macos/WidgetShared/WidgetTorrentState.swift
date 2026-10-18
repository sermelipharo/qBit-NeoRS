import Foundation

/// The few torrent states a widget distinguishes. Raw values are short
/// because they are written into the shared snapshot for every torrent.
enum WidgetTorrentState: String, Codable, Hashable, Sendable, CaseIterable {
    case downloading = "dl"
    case stalledDownloading = "sdl"
    case metadata = "meta"
    case seeding = "up"
    case stalledSeeding = "sup"
    case queued = "q"
    case checking = "chk"
    case moving = "mv"
    case stopped = "stop"
    case completed = "done"
    case error = "err"
    case missingFiles = "miss"

    /// Errored or missing files: the torrent needs attention in the app.
    var isProblem: Bool { self == .error || self == .missingFiles }

    /// Can be stopped (anything but a stopped, completed or broken torrent).
    var isRunning: Bool {
        switch self {
        case .stopped, .completed, .error, .missingFiles: false
        default: true
        }
    }

    /// Fetching data or metadata (the "Downloading" widget filter, together
    /// with incomplete torrents that are queued, checking or moving).
    var isDownloadingState: Bool {
        switch self {
        case .downloading, .stalledDownloading, .metadata: true
        default: false
        }
    }

    var isSeedingState: Bool {
        self == .seeding || self == .stalledSeeding
    }

    /// Waiting for something other than the user (sorted after transfers).
    var isPending: Bool {
        switch self {
        case .queued, .checking, .moving: true
        default: false
        }
    }

    /// Download filter rank: transferring states first, stalled and pending last.
    var downloadRank: Int {
        switch self {
        case .downloading: 0
        case .metadata: 1
        case .stalledDownloading: 2
        case .checking, .moving: 3
        case .queued: 4
        default: 5
        }
    }
}
