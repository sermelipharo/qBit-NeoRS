import AppKit

/// The menu bar's root menu. SwiftUI rebuilds its default menu in whatever
/// menu is the main menu whenever its scene graph changes; this menu lets
/// those edits through (SwiftUI reads the items back while it works) and
/// reports them, so the controller can put its own items back right after.
final class MainMenuRoot: NSMenu {
    /// Called after an edit made by someone else than the controller.
    var onForeignEdit: (() -> Void)?
    /// Set while the controller edits the menu itself.
    var isEditing = false

    override func insertItem(_ newItem: NSMenuItem, at index: Int) {
        super.insertItem(newItem, at: index)
        reportEdit()
    }

    override func addItem(_ newItem: NSMenuItem) {
        super.addItem(newItem)
        reportEdit()
    }

    override func removeItem(_ item: NSMenuItem) {
        super.removeItem(item)
        reportEdit()
    }

    override func removeItem(at index: Int) {
        super.removeItem(at: index)
        reportEdit()
    }

    override func removeAllItems() {
        super.removeAllItems()
        reportEdit()
    }

    private func reportEdit() {
        if !isEditing { onForeignEdit?() }
    }
}
