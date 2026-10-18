import Foundation

/// Everything the widgets show, written by the app into the shared App Group
/// container as compact JSON (typically a few KB; bounded by
/// `WidgetTorrentSelector.maxPublished` listed torrents, `maxPinned` pinned
/// torrents with their own history, `maxCandidates` picker entries,
/// `WidgetSpeedHistory.capacity` speed samples and `maxCategories` categories).
struct WidgetSnapshot: Codable, Hashable, Sendable {
    static let currentVersion = 2
    /// Larger files are ignored (a corrupt or foreign file never gets decoded).
    static let maxEncodedSize = 256 * 1_024

    var version = Self.currentVersion
    var generatedAt: Date
    var status: WidgetAppStatus
    var downloadRate: Int64 = 0
    var uploadRate: Int64 = 0
    var altSpeedEnabled = false
    var counts = WidgetCounts()
    var history: [WidgetSpeedSample] = []
    var torrents: [WidgetTorrent] = []
    var categories: [WidgetCategory] = []
    /// Torrents chosen in installed Torrent widgets.
    var pinned: [WidgetTorrentDetail] = []
    /// Pinned torrents the app no longer has.
    var removed: [String] = []
    /// The Torrent widget picker's choices (transfers first, then the most recent).
    var candidates: [WidgetTorrentRef] = []

    /// Whether the widgets' controls can reach the app.
    var acceptsCommands: Bool { status == .running }

    /// Counts for the whole session or for one category.
    func counts(category: String?) -> WidgetCounts {
        guard let category else { return counts }
        return categories.first { $0.name == category }?.counts ?? WidgetCounts()
    }

    func pinned(_ id: String) -> WidgetTorrentDetail? {
        pinned.first { $0.id == id }
    }

    func torrents(filter: WidgetTorrentFilter, category: String?, limit: Int) -> [WidgetTorrent] {
        WidgetTorrentSelector.select(torrents, filter: filter, category: category, limit: limit)
    }

    /// What changed between two snapshots, for rate limiting (`WidgetPublishPolicy`).
    var fingerprint: WidgetFingerprint {
        var structural = Hasher()
        structural.combine(status)
        structural.combine(altSpeedEnabled)
        structural.combine(counts)
        structural.combine(categories)
        var volatile = Hasher()
        volatile.combine(WidgetFingerprint.bucket(downloadRate))
        volatile.combine(WidgetFingerprint.bucket(uploadRate))
        for torrent in torrents {
            structural.combine(torrent.id)
            structural.combine(torrent.name)
            structural.combine(torrent.state)
            volatile.combine(Int(torrent.progress * 100))
            volatile.combine(WidgetFingerprint.bucket(torrent.totalRate))
        }
        // Picker candidates are left out: they ride along with other writes.
        structural.combine(removed)
        for detail in pinned {
            structural.combine(detail.id)
            structural.combine(detail.torrent.name)
            structural.combine(detail.torrent.state)
            structural.combine(detail.errorMessage)
            volatile.combine(Int(detail.torrent.progress * 1_000))
            volatile.combine(WidgetFingerprint.bucket(detail.torrent.downloadRate))
            volatile.combine(WidgetFingerprint.bucket(detail.torrent.uploadRate))
            volatile.combine(detail.seeds)
            volatile.combine(detail.peers)
            volatile.combine(Int(detail.ratio * 100))
        }
        return WidgetFingerprint(structural: structural.finalize(), volatile: volatile.finalize())
    }

    // MARK: Encoding

    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        return try encoder.encode(self)
    }

    /// Decodes a snapshot; nil for oversized, malformed or other-version data.
    static func decode(_ data: Data) -> Self? {
        guard data.count <= maxEncodedSize else { return nil }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        guard let snapshot = try? decoder.decode(Self.self, from: data), snapshot.version == currentVersion else { return nil }
        return snapshot
    }

    enum CodingKeys: String, CodingKey {
        case version = "v", generatedAt = "at", status = "st", downloadRate = "dl", uploadRate = "ul", altSpeedEnabled = "alt"
        case counts = "n", history = "h", torrents = "t", categories = "c", pinned = "p", removed = "r", candidates = "k"
    }
}
