import Foundation

/// A request from a Torrent widget's button, queued as a small file in the
/// App Group container (`WidgetCommandQueue`) because Darwin notifications
/// cannot carry the torrent's hash.
struct WidgetTorrentCommand: Codable, Hashable, Sendable {
    enum Action: String, Codable, Sendable {
        case start, stop
    }

    var torrentID: String
    var action: Action
    var issuedAt: Date

    enum CodingKeys: String, CodingKey {
        case torrentID = "i", action = "a", issuedAt = "at"
    }
}
