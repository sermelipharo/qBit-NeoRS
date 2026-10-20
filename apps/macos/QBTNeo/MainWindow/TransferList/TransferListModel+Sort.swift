import Foundation

/// The main sort key and its direction, as Sort By (toolbar, View menu,
/// Command Palette) edits them. They read and write `sortOrder`, the state
/// the list's column headers edit and that is persisted, so every place
/// stays in sync with the others and with both the list and the cards.
extension TransferListModel {
    var sortColumn: TransferColumn {
        get { sortOrder.first?.column ?? .name }
        set {
            let order = sortOrder.promotingSortKey(TransferSort(newValue)) { $0.column == $1.column }
            if order != sortOrder { sortOrder = order }
        }
    }

    var sortAscending: Bool {
        get { sortOrder.first?.order != .reverse }
        set {
            var order = sortOrder.isEmpty ? [TransferSort(.name)] : sortOrder
            order[0].order = newValue ? .forward : .reverse
            if order != sortOrder { sortOrder = order }
        }
    }
}
