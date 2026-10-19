import Foundation

/// What the main window's detail area shows. Search, RSS and the execution
/// log live in the main window (like Apple Music's sections) rather than in
/// windows of their own; the sidebar's top rows switch between them.
enum MainPane: String, CaseIterable, Hashable, Sendable {
    case transfers, search, rss, log

    var title: String {
        switch self {
        case .transfers: String(localized: "Transfers")
        case .search: String(localized: "Search")
        case .rss: String(localized: "RSS")
        case .log: String(localized: "Log")
        }
    }

    var symbol: String {
        switch self {
        case .transfers: Symbol.arrowUpArrowDown
        case .search: Symbol.magnifyingglass
        case .rss: Symbol.dotRadiowavesUpForward
        case .log: Symbol.textAlignleft
        }
    }
}
