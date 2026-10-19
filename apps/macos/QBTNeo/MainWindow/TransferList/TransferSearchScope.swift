import Foundation

/// What the toolbar filter matches, chosen from the menu on its magnifying glass.
enum TransferSearchScope: Hashable, CaseIterable, Sendable {
    case name
    case savePath
    /// The torrent's tracker hosts.
    case tracker

    var title: String {
        switch self {
        case .name: String(localized: "Name")
        case .savePath: String(localized: "Save Path")
        case .tracker: String(localized: "Trackers")
        }
    }

    /// The filter field's placeholder for this scope.
    var prompt: String {
        switch self {
        case .name: String(localized: "Filter torrents…")
        case .savePath: String(localized: "Filter by save path…")
        case .tracker: String(localized: "Filter by tracker…")
        }
    }
}
