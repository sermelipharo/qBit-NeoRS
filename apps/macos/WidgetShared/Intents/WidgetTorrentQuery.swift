import AppIntents

/// The Torrent widget's picker. It reads the choices the app published in
/// the shared snapshot (the extension never talks to the core): active and
/// recent torrents are suggested, and typing searches their names.
struct WidgetTorrentQuery: EntityStringQuery {
    /// Suggestions shown before typing.
    static let suggestionLimit = 30

    func entities(for identifiers: [WidgetTorrentEntity.ID]) async throws -> [WidgetTorrentEntity] {
        let snapshot = WidgetSnapshotStore.load()
        return identifiers.map { id in
            let name = snapshot?.pinned(id)?.torrent.name ?? snapshot?.candidates.first { $0.id == id }?.name
            return WidgetTorrentEntity(id: id, name: name ?? String(localized: "Torrent \(String(id.prefix(8)))"))
        }
    }

    func entities(matching string: String) async throws -> [WidgetTorrentEntity] {
        let candidates = WidgetSnapshotStore.load()?.candidates ?? []
        return WidgetTorrentSelector.search(candidates, for: string).map { WidgetTorrentEntity(id: $0.id, name: $0.name) }
    }

    func suggestedEntities() async throws -> [WidgetTorrentEntity] {
        let candidates = WidgetSnapshotStore.load()?.candidates ?? []
        return candidates.prefix(Self.suggestionLimit).map { WidgetTorrentEntity(id: $0.id, name: $0.name) }
    }
}
