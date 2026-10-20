import AppKit

/// Delegate of View › Sort By: when the list is sorted by a column that is
/// not one of the menu's fields (chosen in the list's header), shows that
/// column, checked, so the menu always tells the current sort field.
@MainActor
final class SortMenuUpdater: NSObject, NSMenuDelegate {
    private let list: TransferListModel
    private let otherItem: NSMenuItem

    init(list: TransferListModel, otherItem: NSMenuItem) {
        self.list = list
        self.otherItem = otherItem
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        let column = list.sortColumn
        let isOther = !TransferColumn.sortFields.contains(column)
        if otherItem.isHidden == isOther { otherItem.isHidden = !isOther }
        if isOther, otherItem.title != column.sortTitle { otherItem.title = column.sortTitle }
    }
}
