import AppIntents

/// Offers the categories the app published (the ones with the most torrents).
struct CategoryQuery: EntityQuery {
    func entities(for identifiers: [CategoryEntity.ID]) async throws -> [CategoryEntity] {
        identifiers.map(CategoryEntity.init(id:))
    }

    func suggestedEntities() async throws -> [CategoryEntity] {
        (WidgetSnapshotStore.load()?.categories ?? []).map { CategoryEntity(id: $0.name) }
    }
}
