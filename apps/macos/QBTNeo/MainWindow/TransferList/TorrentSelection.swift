import Foundation

/// Aggregates over the selected torrents that decide which context-menu
/// entries appear and their checked / mixed states (features.md §1.8).
@MainActor
struct TorrentSelection {
    let model: AppModel
    let items: [TorrentItem]

    init(model: AppModel, ids: Set<String>) {
        self.model = model
        items = model.list.visibleItems.filter { ids.contains($0.id) }
    }

    var ids: [String] { items.map(\.id) }
    var isEmpty: Bool { items.isEmpty }

    var needsStart: Bool {
        items.contains { $0.state.isStopped || $0.state.isForced || $0.state.isErrored || $0.state.isChecking }
    }

    var needsStop: Bool { items.contains { !$0.state.isStopped || $0.state.isChecking } }
    var needsForce: Bool { items.contains { !$0.state.isForced || $0.state.isErrored } }
    var hasMetadata: Bool { items.contains(where: \.hasMetadata) }
    var hasUnfinished: Bool { items.contains { !$0.isFinished } }
    var allFinishedWithMetadata: Bool { items.allSatisfy(\.isFinished) && hasMetadata }
    var hasInfohashV1: Bool { items.contains { !$0.infohashV1.isEmpty } }
    var hasInfohashV2: Bool { items.contains { !$0.infohashV2.isEmpty } }
    var canReannounce: Bool { model.canReannounce(items) }

    /// The category shared by every selected torrent, if any.
    var commonCategory: String? {
        guard let first = items.first?.category else { return nil }
        return items.allSatisfy { $0.category == first } ? first : nil
    }
}
