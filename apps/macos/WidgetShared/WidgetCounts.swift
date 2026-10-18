import Foundation

/// Torrent counts shown by the widgets (all torrents, or one category).
struct WidgetCounts: Codable, Hashable, Sendable {
    var total = 0
    /// Transferring data now.
    var active = 0
    var downloading = 0
    var seeding = 0

    mutating func add(_ torrent: some WidgetRankable) {
        total += 1
        if torrent.isTransferring { active += 1 }
        if torrent.isDownloading { downloading += 1 }
        if torrent.state.isSeedingState { seeding += 1 }
    }

    /// The number a widget shows next to the given filter.
    func count(for filter: WidgetTorrentFilter) -> Int {
        switch filter {
        case .active: active
        case .downloading: downloading
        case .seeding: seeding
        case .recent, .all: total
        }
    }

    enum CodingKeys: String, CodingKey {
        case total = "t", active = "a", downloading = "d", seeding = "s"
    }
}
