import AppKit

/// Context menus of the sidebar rows (features.md §1.5): category and tag
/// management, Remove Tracker on hosts, and Start/Stop/Remove torrents,
/// which act on the torrents currently visible in the list.
@MainActor
enum SidebarMenu {
    static func make(for item: SidebarItem, model: AppModel) -> NSMenu {
        let menu = NSMenu.manual()
        switch item {
        case .category(let filter):
            menu.addAction(String(localized: "Add Category…"), symbol: Symbol.plus) { model.requestNewCategory() }
            if case .category(let category) = filter {
                menu.addAction(String(localized: "Add Subcategory…"), symbol: Symbol.folderBadgePlus) {
                    model.requestNewCategory(parent: category)
                }
                menu.addAction(String(localized: "Edit Category…"), symbol: Symbol.pencil) { model.requestEditCategory(category) }
                menu.addAction(String(localized: "Remove Category"), symbol: Symbol.minusCircle) { model.removeCategory(category) }
            }
            menu.addAction(String(localized: "Remove Unused Categories"), symbol: Symbol.trashSlash) { model.removeUnusedCategories() }
            menu.addDivider()
        case .tag(let filter):
            menu.addAction(String(localized: "Add Tag…"), symbol: Symbol.plus) { model.requestAddTag() }
            if case .tag(let tag) = filter {
                menu.addAction(String(localized: "Edit Tag…"), symbol: Symbol.pencil) { model.requestEditTag(tag) }
                menu.addAction(String(localized: "Remove Tag"), symbol: Symbol.minusCircle) { model.removeTag(tag) }
            }
            menu.addAction(String(localized: "Remove Unused Tags"), symbol: Symbol.trashSlash) { model.removeUnusedTags() }
            menu.addDivider()
        case .tracker(.host(let host)):
            menu.addAction(String(localized: "Remove Tracker"), symbol: Symbol.minusCircle) { model.requestRemoveTracker(host: host) }
            menu.addDivider()
        case .status, .trackerStatus, .tracker:
            break
        }
        menu.addAction(String(localized: "Start Torrents"), symbol: Symbol.playFill) { model.start(model.visibleIDs) }
        menu.addAction(String(localized: "Stop Torrents"), symbol: Symbol.pauseFill) { model.stop(model.visibleIDs) }
        menu.addAction(String(localized: "Remove Torrents"), symbol: Symbol.trash) { model.requestRemove(model.visibleIDs) }
        return menu
    }
}
