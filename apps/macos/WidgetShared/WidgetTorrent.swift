import Foundation

/// One torrent as published to the widgets. Coding keys are one letter to
/// keep the snapshot small.
struct WidgetTorrent: Codable, Hashable, Sendable, Identifiable, WidgetRankable {
    /// Longest name written; widgets truncate further when drawing.
    static let maxNameLength = 64
    /// Longest identifier written (info hashes are 40 hexadecimal characters).
    static let idLength = 64
    /// qBittorrent's "infinite" ETA (100 days).
    static let infiniteETA: Int64 = 8_640_000

    var id: String
    var name: String
    var state: WidgetTorrentState
    var progress: Double
    var downloadRate: Int64
    var uploadRate: Int64
    /// Seconds; negative or `infiniteETA` when unknown.
    var eta: Int64
    var size: Int64
    var category: String
    var addedOn: Int64
    var completedOn: Int64

    init(id: String, name: String, state: WidgetTorrentState, progress: Double, downloadRate: Int64, uploadRate: Int64,
         eta: Int64, size: Int64, category: String, addedOn: Int64, completedOn: Int64) {
        self.id = String(id.prefix(Self.idLength))
        self.name = name.count > Self.maxNameLength ? String(name.prefix(Self.maxNameLength - 1)) + "…" : name
        self.state = state
        self.progress = progress.isFinite ? min(max(progress, 0), 1) : 0
        self.downloadRate = max(downloadRate, 0)
        self.uploadRate = max(uploadRate, 0)
        self.eta = eta
        self.size = size
        self.category = category
        self.addedOn = addedOn
        self.completedOn = completedOn
    }

    /// The remaining time, when it is known and finite.
    var remainingTime: Int64? {
        eta >= 0 && eta < Self.infiniteETA && progress < 1 ? eta : nil
    }

    enum CodingKeys: String, CodingKey {
        case id = "i", name = "n", state = "s", progress = "p", downloadRate = "d", uploadRate = "u", eta = "e", size = "z"
        case category = "c", addedOn = "a", completedOn = "f"
    }
}
