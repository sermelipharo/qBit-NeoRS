import Foundation

/// A torrent offered in the Torrent widget's picker: identifier and name
/// only, encoded as `[id, name]`.
struct WidgetTorrentRef: Codable, Hashable, Sendable, Identifiable {
    static let maxNameLength = 48

    var id: String
    var name: String

    init(id: String, name: String) {
        self.id = String(id.prefix(WidgetTorrent.idLength))
        self.name = WidgetText.bounded(name, Self.maxNameLength)
    }

    init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        self.init(id: try container.decode(String.self), name: try container.decode(String.self))
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.unkeyedContainer()
        try container.encode(id)
        try container.encode(name)
    }
}
