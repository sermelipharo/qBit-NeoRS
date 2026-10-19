import Foundation

/// A torrent pinned by a Torrent widget: everything that widget shows,
/// including its own speed history. Only torrents chosen in an installed
/// widget's configuration are published this way (`WidgetTorrentSelector.maxPinned`).
struct WidgetTorrentDetail: Codable, Hashable, Sendable, Identifiable {
    static let maxTags = 5
    static let maxTagLength = 32
    static let maxLocationLength = 48
    static let maxErrorLength = 120
    static let maxChips = 8
    static let maxTitleLength = 160

    var torrent: WidgetTorrent
    /// Bytes of the wanted files already downloaded.
    var downloaded: Int64
    var ratio: Double
    var seeds: Int64
    var seedsInSwarm: Int64
    var peers: Int64
    var peersInSwarm: Int64
    /// Distributed copies; negative when unknown.
    var availability: Double
    var tags: [String]
    /// The last folders of the save path ("Downloads/Linux").
    var location: String
    var errorMessage: String
    var history: [WidgetSpeedSample]
    /// The name's title and technical tags, split by the app's name rules
    /// as on the cards; nil when names are shown whole.
    var title: String?
    var chips: [WidgetNameChip]?

    var id: String { torrent.id }

    /// The title to show (the widget's torrent picker keeps the full name).
    var displayTitle: String { title ?? torrent.name }

    init(torrent: WidgetTorrent, downloaded: Int64, ratio: Double, seeds: Int64, seedsInSwarm: Int64, peers: Int64, peersInSwarm: Int64,
         availability: Double, tags: [String], savePath: String, errorMessage: String, history: [WidgetSpeedSample],
         title: String? = nil, chips: [WidgetNameChip] = []) {
        self.torrent = torrent
        self.downloaded = max(downloaded, 0)
        self.ratio = ratio.isFinite ? max(ratio, 0) : 0
        self.seeds = seeds
        self.seedsInSwarm = seedsInSwarm
        self.peers = peers
        self.peersInSwarm = peersInSwarm
        self.availability = availability.isFinite ? availability : -1
        self.tags = tags.prefix(Self.maxTags).map { WidgetText.bounded($0, Self.maxTagLength) }
        location = WidgetText.bounded(WidgetText.pathTail(savePath), Self.maxLocationLength, keepingEnd: true)
        self.errorMessage = WidgetText.bounded(errorMessage, Self.maxErrorLength)
        self.history = Array(history.suffix(WidgetSpeedHistory.capacity))
        self.title = title.flatMap { $0.isEmpty ? nil : WidgetText.bounded($0, Self.maxTitleLength) }
        self.chips = chips.isEmpty ? nil : Array(chips.prefix(Self.maxChips))
    }

    enum CodingKeys: String, CodingKey {
        case torrent = "t", downloaded = "dn", ratio = "r", seeds = "s", seedsInSwarm = "sw", peers = "p", peersInSwarm = "pw"
        case availability = "av", tags = "g", location = "l", errorMessage = "e", history = "h", title = "n", chips = "c"
    }
}
