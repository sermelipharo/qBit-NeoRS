import Foundation

/// One selectable sidebar row. The sidebar keeps exactly one selected row per
/// section; all section filters combine with AND (qBittorrent semantics).
enum SidebarItem: Hashable, Sendable {
    case status(StatusFilter)
    case category(CategoryFilter)
    case tag(TagFilter)
    case trackerStatus(TrackerStatusFilter)
    case tracker(TrackerFilter)

    enum Section: String, CaseIterable, Hashable, Sendable {
        case status, category, tag, trackerStatus, tracker
        /// The RSS feeds and folders (not a transfer filter).
        case rss

        var title: String {
            switch self {
            case .status: String(localized: "Status")
            case .category: String(localized: "Categories")
            case .tag: String(localized: "Tags")
            case .trackerStatus: String(localized: "Tracker Status")
            case .tracker: String(localized: "Trackers")
            case .rss: String(localized: "RSS")
            }
        }
    }

    var section: Section {
        switch self {
        case .status: .status
        case .category: .category
        case .tag: .tag
        case .trackerStatus: .trackerStatus
        case .tracker: .tracker
        }
    }
}
