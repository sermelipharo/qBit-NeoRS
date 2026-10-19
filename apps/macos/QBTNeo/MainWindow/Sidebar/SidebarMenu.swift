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
        menu.addDivider()
        addHide(item.section, to: menu, model: model)
        return menu
    }

    /// A section header: hide the whole section (View › Sidebar brings it back).
    static func make(forSection section: SidebarItem.Section, model: AppModel) -> NSMenu {
        let menu = NSMenu.manual()
        if section == .rss {
            addFeedActions(to: menu, model: model)
            menu.addDivider()
        }
        addHide(section, to: menu, model: model)
        return menu
    }

    /// An RSS feed or folder: the reader's feed actions, on it (features.md §5.12.1).
    static func make(forFeed id: String, model: AppModel) -> NSMenu {
        let menu = NSMenu.manual()
        let rss = model.rssTab
        if let node = rss.node(id) {
            menu.addAction(String(localized: "Update"), symbol: Symbol.arrowClockwise) { rss.update([node]) }
            menu.addAction(String(localized: "Mark Items Read"), symbol: Symbol.checkmarkCircle) { rss.markRead([node]) }
            menu.addDivider()
            menu.addAction(String(localized: "Rename…"), symbol: Symbol.pencil) { openRSS(model) { rss.sheet = .rename(node) } }
            if node.kind == .feed {
                menu.addAction(String(localized: "Feed Options…"), symbol: Symbol.gearshape) { openRSS(model) { rss.sheet = .feedOptions(node) } }
                menu.addAction(String(localized: "Copy Feed URL"), symbol: Symbol.docOnDoc) { rss.copyFeedURLs([node]) }
            }
            menu.addAction(String(localized: "Delete"), symbol: Symbol.trash) { openRSS(model) { rss.pendingDeletion = [node] } }
            menu.addDivider()
        }
        addFeedActions(to: menu, model: model)
        menu.addDivider()
        addHide(.rss, to: menu, model: model)
        return menu
    }

    private static func addFeedActions(to menu: NSMenu, model: AppModel) {
        let rss = model.rssTab
        menu.addAction(String(localized: "New Subscription…"), symbol: Symbol.plus) {
            openRSS(model) { rss.sheet = .newSubscription(url: RSSFeedSheet.clipboardURL()) }
        }
        menu.addAction(String(localized: "New Folder…"), symbol: Symbol.folderBadgePlus) { openRSS(model) { rss.sheet = .newFolder } }
        menu.addAction(String(localized: "Update All Feeds"), symbol: Symbol.arrowClockwise) { rss.updateAll() }
    }

    /// Sheets and confirmations of the reader are presented by the RSS tab.
    private static func openRSS(_ model: AppModel, then action: @escaping @MainActor () -> Void) {
        model.show(pane: .rss)
        action()
    }

    private static func addHide(_ section: SidebarItem.Section, to menu: NSMenu, model: AppModel) {
        let title = String(localized: "sidebar.hideSection", defaultValue: "Hide \(section.title)",
                           comment: "Sidebar context menu: hides a whole section, e.g. Hide Trackers")
        menu.addAction(title, symbol: Symbol.eyeSlash) { model.setSidebarSection(section, hidden: true) }
    }
}
