import AppIntents

/// A torrent a Torrent widget follows, identified by its info hash.
struct WidgetTorrentEntity: AppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Torrent"
    static let defaultQuery = WidgetTorrentQuery()

    /// The info hash.
    var id: String
    var name: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(title: "\(name)")
    }
}
