import Foundation

/// Which torrents a Transfers widget lists.
enum WidgetTorrentFilter: String, Codable, Hashable, Sendable, CaseIterable {
    /// Transferring now, fastest first.
    case active
    /// Incomplete and not stopped, fastest first.
    case downloading
    /// Seeding, fastest upload first.
    case seeding
    /// Most recently added or completed first.
    case recent
    /// Everything: transfers first, then the most recent.
    case all
}
