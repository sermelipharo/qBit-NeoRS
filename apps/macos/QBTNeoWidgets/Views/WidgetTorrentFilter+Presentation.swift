import Foundation

extension WidgetTorrentFilter {
    var title: String {
        switch self {
        case .active: String(localized: "Active")
        case .downloading: String(localized: "Downloading")
        case .seeding: String(localized: "Seeding")
        case .recent: String(localized: "Recent")
        case .all: String(localized: "All")
        }
    }

    /// Shown when nothing matches.
    var emptyMessage: String {
        switch self {
        case .active: String(localized: "Nothing is transferring")
        case .downloading: String(localized: "No downloads")
        case .seeding: String(localized: "Nothing is seeding")
        case .recent, .all: String(localized: "No torrents")
        }
    }

    var emptySymbol: String {
        switch self {
        case .active, .recent, .all: "tray"
        case .downloading: "arrow.down.circle"
        case .seeding: "arrow.up.circle"
        }
    }

    /// "3 of 12 active", "4 downloading", "12 torrents".
    func countText(_ counts: WidgetCounts) -> String {
        switch self {
        case .active: String(localized: "\(counts.active) of \(counts.total) active")
        case .downloading: String(localized: "\(counts.downloading) downloading")
        case .seeding: String(localized: "\(counts.seeding) seeding")
        case .recent, .all: String(localized: "\(counts.total) torrents")
        }
    }
}
