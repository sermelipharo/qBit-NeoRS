import SwiftUI

/// The transfer list (an AppKit table, see `TransferTableController`, or
/// cards, see `TransferCardsController`) with its empty-state placeholder.
/// Only the chosen view exists. Drops on the placeholder add torrents too.
struct TransferTable: View {
    @Environment(AppModel.self) private var model
    @Environment(GUIPreferences.self) private var gui

    var body: some View {
        Group {
            switch gui.transferViewMode {
            case .list: TransferTableView(model: model)
            case .cards: TransferCardsView(model: model)
            }
        }
        .overlay {
            if model.list.visibleItems.isEmpty, model.phase == .running {
                TransferListEmptyView(isFiltering: !model.transfers.items.isEmpty)
                    .dropDestination(for: DroppedItem.self) { items, _ in
                        _ = model.handleDrop(items)
                    }
            }
        }
    }
}
