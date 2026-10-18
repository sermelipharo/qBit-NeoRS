import Foundation

/// The Torrent widget's text lines.
enum TorrentDetailText {
    /// Speed and time left while downloading, upload speed while seeding,
    /// otherwise the state.
    static func activity(_ detail: WidgetTorrentDetail) -> String {
        let torrent = detail.torrent
        switch torrent.state {
        case .downloading, .stalledDownloading, .metadata:
            var parts: [String] = []
            if torrent.downloadRate > 0 { parts.append("↓ " + WidgetFormat.speed(torrent.downloadRate)) }
            if let eta = torrent.remainingTime { parts.append(WidgetFormat.eta(eta)) }
            return parts.isEmpty ? torrent.state.title : parts.joined(separator: " · ")
        case .seeding, .stalledSeeding:
            return "↑ " + WidgetFormat.speed(torrent.uploadRate)
        case .error, .missingFiles:
            return detail.errorMessage.isEmpty ? torrent.state.title : detail.errorMessage
        default:
            return torrent.state.title
        }
    }

    /// "2.9 GiB of 4.7 GiB", or just the size once complete.
    static func size(_ detail: WidgetTorrentDetail) -> String {
        let total = UnitFormatter.size(max(detail.torrent.size, 0))
        guard detail.torrent.progress < 1 else { return total }
        return String(localized: "\(UnitFormatter.size(detail.downloaded)) of \(total)")
    }

    static func ratio(_ detail: WidgetTorrentDetail) -> String {
        detail.ratio.formatted(.number.precision(.fractionLength(2)))
    }

    /// "12 (40)": connected (in the swarm, when known).
    static func connections(_ connected: Int64, _ swarm: Int64) -> String {
        swarm >= 0 ? "\(connected) (\(swarm))" : "\(connected)"
    }

    static func availability(_ detail: WidgetTorrentDetail) -> String {
        detail.availability < 0 ? "–" : detail.availability.formatted(.number.precision(.fractionLength(2)))
    }

    /// "Ratio 0.42 · Seeds 12 (40) · Peers 3 (9)".
    static func swarm(_ detail: WidgetTorrentDetail) -> String {
        String(localized: """
            Ratio \(ratio(detail)) · Seeds \(connections(detail.seeds, detail.seedsInSwarm)) · \
            Peers \(connections(detail.peers, detail.peersInSwarm))
            """)
    }

    /// Category and tags, e.g. "Linux ISOs · ubuntu, lts".
    static func labels(_ detail: WidgetTorrentDetail) -> String {
        var parts: [String] = []
        if !detail.torrent.category.isEmpty { parts.append(detail.torrent.category) }
        if !detail.tags.isEmpty { parts.append(detail.tags.joined(separator: ", ")) }
        return parts.joined(separator: " · ")
    }

    /// When the download finished, if known.
    static func completedDate(_ detail: WidgetTorrentDetail) -> Date? {
        detail.torrent.completedOn > 0 ? Date(timeIntervalSince1970: TimeInterval(detail.torrent.completedOn)) : nil
    }
}
